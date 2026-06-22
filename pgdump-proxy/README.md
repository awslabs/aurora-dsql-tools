# DSQL pg_dump proxy

A tiny, dependency-free wire proxy that lets stock **`pg_dump`** and **`psql`**
read an [Amazon Aurora DSQL](https://aws.amazon.com/rds/aurora/dsql/) cluster.

## Why this exists

Aurora DSQL speaks the PostgreSQL wire protocol, but supports only a fixed
allowlist of session parameters and a subset of statements. On connection,
`pg_dump` unconditionally issues several statements DSQL rejects, and it aborts
on the first error:

```
SET statement_timeout = 0                     -> ERROR: not supported
SET lock_timeout = 0                          -> ERROR: not supported
SET idle_in_transaction_session_timeout = 0   -> ERROR: not supported
SET synchronize_seqscans = off                -> ERROR: not supported  (pg_dump dies here)
SET row_security = off                        -> ERROR: not supported
SET standard_conforming_strings = on          -> ERROR: not supported
SELECT set_config('restrict_nonsystem_relation_kind', ...)  -> ERROR: not supported
LOCK TABLE ... IN ACCESS SHARE MODE           -> ERROR: unsupported statement: Lock
```

There is no `pg_dump` flag to suppress these, so **you cannot dump a DSQL
cluster with stock tooling** — which blocks any pg_dump-based DSQL→DSQL
migration.

## How it works

The proxy sits between the client (plaintext, on localhost) and DSQL (TLS):

- Startup and authentication bytes are **passed through untouched** — DSQL's IAM
  token is just a cleartext password, so the proxy never terminates auth.
- It intercepts only the setup statements DSQL rejects, none of which affect the
  dump's *content*:
  - a rejected `SET <param>` → a synthesized `SET` success reply,
  - `SELECT set_config('<rejected>', ...)` → rewritten to `SELECT NULL::text`,
  - `LOCK TABLE ...` → a synthesized `LOCK TABLE` reply (DSQL gives snapshot
    isolation, so the lock is unnecessary).
- Content-relevant GUCs (`client_encoding`, `DateStyle`, `extra_float_digits`,
  `intervalstyle`, `timezone`, `search_path`) are on DSQL's allowlist and pass
  through, so dump fidelity is preserved.

Pure Python standard library — no third-party dependencies.

## Usage

```bash
# 1. Start the proxy (listens on 127.0.0.1:6543 by default):
python3 dsql_pgdump_proxy.py <cluster-endpoint>

# 2. In another shell, dump through it. The password is a DSQL auth token; the
#    client->proxy hop is plaintext-localhost, so sslmode does not matter.
export PGPASSWORD="$(aws dsql generate-db-connect-admin-auth-token \
    --hostname <cluster-endpoint> --region <region> --expires-in 3600)"

pg_dump -Fp --no-owner --no-privileges \
    "host=127.0.0.1 port=6543 dbname=postgres user=admin" > dump.sql
```

The result is a plain (`-Fp`) pg_dump. Feed it to the
[Aurora DSQL Loader](https://github.com/aws-samples/aurora-dsql-loader)'s
`migrate` command to apply it back into a DSQL cluster — the loader uses
[dsql-lint](../dsql-lint/) to collapse the DSQL-native identity / compression
idioms a DSQL dump contains.

Options: `--target-port` (default 5432), `--listen-host` (default `127.0.0.1`),
`--listen-port` (default 6543). Run `python3 dsql_pgdump_proxy.py --help`.

> **Keep the listen address on loopback.** The client→proxy hop is plaintext, so
> the DSQL auth token and dump data cross it unencrypted. The default
> `127.0.0.1` keeps that on-host; binding `--listen-host` to a non-loopback
> address exposes both on the network (the proxy warns when you do).

## Limitations

- **Plain format only.** Use `pg_dump -Fp` (the default). The proxy does not
  change what pg_dump emits; custom/directory archives are a pg_dump concern.
- **One auth token per session, max 1 hour.** Generate a fresh token if a dump
  of a very large cluster outlives the token's `--expires-in`.
- **Single host.** The proxy targets one cluster endpoint per run; start one per
  cluster if dumping several.
- **Use for migration/export, not as a general-purpose gateway.** It rewrites
  only the statements needed for `pg_dump`/`psql` to function against DSQL.
- **Simple-query setup only.** Interception fires on simple-query (`'Q'`)
  messages carrying one setup statement — what `pg_dump`/`psql` actually send.
  Setup statements issued via the extended-query protocol (Parse/Bind/Execute) or
  bundled in a multi-statement `'Q'` batch (`SET x; SELECT ...`) are passed
  through untouched; if DSQL rejects one, the connection aborts. The alternate
  `SET TIME ZONE '...'` spelling is classified by its first identifier (`TIME`)
  and swallowed like any other unsupported SET; pg_dump emits the allowlisted
  `SET timezone = '...'` form, so the export path's timezone fidelity holds. The
  supported `pg_dump`/`psql` export path does not hit these cases.
