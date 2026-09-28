#!/usr/bin/env python3
"""Let stock `pg_dump` / `psql` read an Aurora DSQL cluster.

Aurora DSQL speaks the PostgreSQL wire protocol but supports only a fixed
allowlist of session parameters and a subset of statements. `pg_dump` issues a
handful of connection-setup statements that DSQL rejects ("setting configuration
parameter X not supported" / "unsupported statement: Lock"), and pg_dump aborts
on the first error — so you cannot dump a DSQL cluster with stock tooling.

This proxy sits between the client (plaintext, on localhost) and DSQL (TLS). It
passes the startup and authentication bytes through untouched — DSQL's IAM token
is just a cleartext password, so the proxy never terminates auth. For setup
statements that do not affect dump content, it synthesizes or rewrites the
rejected operation:

  * `SET <param>` for a param DSQL rejects        -> synth a `SET` success reply
  * `SELECT ... set_config(...)` setup probe       -> rewrite to `SELECT NULL::text`
  * `LOCK TABLE ... IN ... MODE`                  -> synth a `LOCK TABLE` reply
                                                     (DSQL is snapshot-isolated;
                                                      the lock is unnecessary)

pg_dump's catalog `PREPARE` / `EXECUTE` pairs are different: their query results
do affect dump content. For the known pg_dump one-OID catalog queries, the proxy
retains the SELECT locally and sends that same SELECT to DSQL when EXECUTE
arrives, with only the OID parameter safely inlined.

Content-relevant GUCs (`client_encoding`, `DateStyle`, `extra_float_digits`,
`intervalstyle`, `timezone`, `search_path`) are on DSQL's allowlist and pass
through, so dump fidelity is preserved.

Interception is scoped to single-statement simple queries (`'Q'`) — what
`pg_dump`/`psql` setup emits. Extended-query (Parse/Bind/Execute) and
multi-statement `'Q'` batches are passed through and, if DSQL rejects them, abort
the connection. Note the `SET RE` captures the first identifier, so the alternate
`SET TIME ZONE '...'` spelling matches param `TIME` (not allowlisted) and is
swallowed like any other unsupported SET — pg_dump itself emits the allowlisted
`SET timezone = '...'` GUC form, so the export path's timezone fidelity is intact.
Fine for the export path; not a general gateway.

Usage:
    # 1. Start the proxy (defaults to 127.0.0.1:6543 -> <endpoint>:5432):
    python3 dsql_pgdump_proxy.py <cluster-endpoint>

    # 2. In another shell, point pg_dump at the proxy. The password is a DSQL
    #    auth token; the proxy connection is plaintext-localhost so sslmode is
    #    irrelevant to the client.
    export PGPASSWORD="$(aws dsql generate-db-connect-admin-auth-token \\
        --hostname <cluster-endpoint> --region <region> --expires-in 3600)"
    pg_dump -Fp --no-owner --no-privileges \\
        "host=127.0.0.1 port=6543 dbname=postgres user=admin" > dump.sql

The dump is a plain pg_dump that the Aurora DSQL Loader's `migrate` command can
apply back into a DSQL cluster (see github.com/aws-samples/aurora-dsql-loader),
which uses dsql-lint to collapse the DSQL-native identity / compression idioms.

Pure standard library; no third-party dependencies.
"""
from __future__ import annotations  # `X | None` annotations on Python 3.9

import argparse
import re
import socket
import ssl
import struct
import sys
import threading

# Session parameters DSQL accepts via SET (per the "Supported session
# parameters" docs). `enable_*` planner toggles and `disable_sync_create_index`
# are also accepted and matched by prefix below. Anything else is fake-accepted
# so pg_dump's setup SETs (statement_timeout, synchronize_seqscans, row_security,
# standard_conforming_strings, ...) don't abort the connection.
ALLOWED_SET_PARAMS = {
    "application_name", "client_encoding", "datestyle", "extra_float_digits",
    "intervalstyle", "timezone", "search_path", "role",
}

SET_RE = re.compile(
    rb'^\s*SET\s+(?:SESSION\s+|LOCAL\s+)?"?([A-Za-z_][A-Za-z0-9_]*)',
    re.IGNORECASE,
)
# pg_dump's set_config setup probe. It comes in two shapes, both of which DSQL
# rejects and both of which must be neutralized:
#   SELECT pg_catalog.set_config('search_path', '', false);
#   SELECT set_config(name, '...', false) FROM pg_settings WHERE name = '...'
# (the second sets restrict_nonsystem_relation_kind via a pg_settings lookup, so
# the param is not a literal first arg). Anchored to a leading SELECT so a
# `set_config(` substring inside a string literal or column ref is not matched;
# the whole probe is rewritten to `SELECT NULL::text` regardless of the named param
# (none of pg_dump's setup set_config calls affect dump content).
SET_CONFIG_RE = re.compile(
    rb"^\s*SELECT\s+(?:pg_catalog\.)?set_config\s*\(", re.IGNORECASE)
LOCK_RE = re.compile(rb'^\s*LOCK\b', re.IGNORECASE)

# pg_dump prepares these object-detail catalog queries with one `pg_catalog.oid`
# parameter, then executes them with a decimal OID. SQL-level PREPARE is
# intentionally unsupported by DSQL, so retain only these pg_dump-owned queries
# in the proxy and inline the typed OID when EXECUTE arrives. This is deliberately
# not a general PREPARE implementation: user statements and unfamiliar pg_dump
# query shapes still pass through to DSQL and fail normally.
PGDUMP_OID_PREPARED_QUERIES = {
    b"getdomainconstraints": b"pg_constraint",
    b"dumpenumtype": b"pg_enum",
    b"dumprangetype": b"pg_range",
    b"dumpbasetype": b"pg_type",
    b"dumpdomain": b"pg_type",
    b"dumpcompositetype": b"pg_attribute",
    b"dumpfunc": b"pg_proc",
    b"dumpopr": b"pg_operator",
    b"dumpagg": b"pg_aggregate",
    b"getcolumnacls": b"pg_attribute",
    b"dumptableattach": b"pg_class",
}
PGDUMP_PREPARE_RE = re.compile(
    rb"^\s*PREPARE\s+([A-Za-z_][A-Za-z0-9_]*)\s*"
    rb"\(\s*pg_catalog\.oid\s*\)\s+AS\s+(SELECT\b.*)\s*$",
    re.IGNORECASE | re.DOTALL,
)
PGDUMP_EXECUTE_RE = re.compile(
    rb"^\s*EXECUTE\s+([A-Za-z_][A-Za-z0-9_]*)\s*"
    rb"\(\s*'([0-9]+)'\s*\)\s*;?\s*$",
    re.IGNORECASE,
)
SSL_REQUEST_CODE = 80877103
GSS_ENC_REQUEST_CODE = 80877104
MAX_POSTGRES_OID = (1 << 32) - 1


def set_param_allowed(param: bytes) -> bool:
    p = param.decode("ascii", "replace").lower()
    return (
        p in ALLOWED_SET_PARAMS
        or p.startswith("enable_")
        or p == "disable_sync_create_index"
    )


def _ascii_alpha(value: int) -> bool:
    return ord("A") <= value <= ord("Z") or ord("a") <= value <= ord("z")


def _ascii_digit(value: int) -> bool:
    return ord("0") <= value <= ord("9")


def _ascii_alnum(value: int) -> bool:
    return _ascii_alpha(value) or _ascii_digit(value)


def _skip_quoted(sql: bytes, start: int, quote: int,
                 backslash_escapes: bool = False) -> int | None:
    """Return the offset after a quoted SQL token, or None if it is unclosed."""
    i = start + 1
    while i < len(sql):
        if backslash_escapes and sql[i] == ord("\\"):
            i += 2
            continue
        if sql[i] == quote:
            if i + 1 < len(sql) and sql[i + 1] == quote:
                i += 2
                continue
            return i + 1
        i += 1
    return None


def _scan_sql(
    sql: bytes,
) -> tuple[list[bytes], list[tuple[int, int, bytes]], list[int]] | None:
    """Scan SQL outside literals, quoted identifiers, and comments.

    Returns lowercase word/punctuation tokens, positional-parameter spans, and
    structural semicolon offsets. This is intentionally a small lexical scanner,
    not a general SQL parser.
    """
    tokens: list[bytes] = []
    parameters: list[tuple[int, int, bytes]] = []
    semicolons: list[int] = []
    i = 0
    while i < len(sql):
        c = sql[i]
        if c in b" \t\r\n\f":
            i += 1
            continue
        if c == ord("'"):
            end = _skip_quoted(sql, i, c)
            if end is None:
                return None
            i = end
            continue
        if c == ord('"'):
            end = _skip_quoted(sql, i, c)
            if end is None:
                return None
            i = end
            continue
        if sql.startswith(b"--", i):
            newline = sql.find(b"\n", i + 2)
            i = len(sql) if newline < 0 else newline + 1
            continue
        if sql.startswith(b"/*", i):
            depth = 1
            i += 2
            while i < len(sql) and depth:
                if sql.startswith(b"/*", i):
                    depth += 1
                    i += 2
                elif sql.startswith(b"*/", i):
                    depth -= 1
                    i += 2
                else:
                    i += 1
            if depth:
                return None
            continue
        if c == ord("$"):
            delimiter: bytes | None = None
            if i + 1 < len(sql) and sql[i + 1] == ord("$"):
                delimiter = b"$$"
            elif i + 1 < len(sql) and (
                sql[i + 1] == ord("_")
                or _ascii_alpha(sql[i + 1])
            ):
                end = i + 2
                while end < len(sql) and (
                    sql[end] == ord("_")
                    or _ascii_alnum(sql[end])
                ):
                    end += 1
                if end < len(sql) and sql[end] == ord("$"):
                    delimiter = sql[i:end + 1]
            if delimiter is not None:
                end = sql.find(delimiter, i + len(delimiter))
                if end < 0:
                    return None
                i = end + len(delimiter)
                continue
            if i + 1 < len(sql) and _ascii_digit(sql[i + 1]):
                end = i + 2
                while end < len(sql) and _ascii_digit(sql[end]):
                    end += 1
                parameter = sql[i:end]
                if end < len(sql) and (
                    sql[end] in (ord("_"), ord("$"))
                    or _ascii_alnum(sql[end])
                ):
                    parameter = b"$invalid"
                parameters.append((i, end, parameter))
                i = end
                continue
        if c == ord("_") or _ascii_alpha(c):
            end = i + 1
            while end < len(sql) and (
                sql[end] in (ord("_"), ord("$"))
                or _ascii_alnum(sql[end])
            ):
                end += 1
            word = sql[i:end].lower()
            tokens.append(word)
            if word == b"e" and end < len(sql) and sql[end] == ord("'"):
                quoted_end = _skip_quoted(
                    sql, end, ord("'"), backslash_escapes=True)
                if quoted_end is None:
                    return None
                i = quoted_end
            else:
                i = end
            continue
        if c in b".,();":
            token = bytes((c,))
            tokens.append(token)
            if c == ord(";"):
                semicolons.append(i)
        i += 1
    return tokens, parameters, semicolons


def _references_catalog(tokens: list[bytes], catalog: bytes) -> bool:
    """Return whether a FROM/JOIN references pg_catalog.<catalog>."""
    for i, token in enumerate(tokens):
        if token not in (b"from", b"join"):
            continue
        j = i + 1
        if j < len(tokens) and tokens[j] == b"only":
            j += 1
        if tokens[j:j + 3] == [b"pg_catalog", b".", catalog]:
            return True
    return False


def _single_oid_parameter(select: bytes) -> tuple[int, int] | None:
    """Return the sole structural $1 span, rejecting other query shapes."""
    scanned = _scan_sql(select)
    if scanned is None:
        return None
    _, parameters, semicolons = scanned
    if semicolons or len(parameters) != 1 or parameters[0][2] != b"$1":
        return None
    return parameters[0][0], parameters[0][1]


def parse_pgdump_prepare(query: bytes) -> tuple[bytes, bytes] | None:
    """Return normalized name and underlying SELECT for a known pg_dump
    one-OID PREPARE statement, or None for every other statement."""
    m = PGDUMP_PREPARE_RE.match(query)
    if not m:
        return None

    name = m.group(1).lower()
    expected_catalog = PGDUMP_OID_PREPARED_QUERIES.get(name)
    if expected_catalog is None:
        return None

    select = m.group(2).strip()
    if select.endswith(b";"):
        select = select[:-1].rstrip()
    scanned = _scan_sql(select)
    if scanned is None:
        return None
    tokens, _, semicolons = scanned
    if (
        semicolons
        or _single_oid_parameter(select) is None
        or not _references_catalog(tokens, expected_catalog)
    ):
        return None
    return name, select


def rewrite_pgdump_execute(
    query: bytes, prepared_queries: dict[bytes, bytes]
) -> tuple[bytes, bytes] | None:
    """Rewrite a matching pg_dump EXECUTE as its underlying DSQL SELECT."""
    m = PGDUMP_EXECUTE_RE.match(query)
    if not m:
        return None

    name = m.group(1).lower()
    select = prepared_queries.get(name)
    if select is None:
        return None

    oid = m.group(2)
    # A PostgreSQL OID is an unsigned 32-bit integer. Checking the digit count
    # before int() also prevents an arbitrarily large EXECUTE argument from
    # consuming disproportionate CPU or memory.
    if len(oid) > 10 or int(oid) > MAX_POSTGRES_OID:
        return None
    parameter = _single_oid_parameter(select)
    if parameter is None:
        return None

    # PREPARE declared $1 as pg_catalog.oid. Preserve that type explicitly
    # after inlining rather than relying on an unknown string literal coercion.
    typed_oid = b"('" + oid + b"'::pg_catalog.oid)"
    start, end = parameter
    return name, select[:start] + typed_oid + select[end:]


def frame(type_byte: bytes, body: bytes) -> bytes:
    """Build a typed protocol message: 1-byte type + Int32 length + body. The
    length covers itself (4) plus the body, per the PostgreSQL wire protocol."""
    return type_byte + struct.pack("!I", 4 + len(body)) + body


def recv_exact(sock: socket.socket, n: int) -> bytes | None:
    """Read exactly `n` bytes, or `None` if the peer closed first."""
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            return None
        buf += chunk
    return buf


def send_command_complete(client: socket.socket, lock: threading.Lock,
                          tag: bytes, status: bytes) -> None:
    """Send CommandComplete(tag) + ReadyForQuery(status) to the client.

    status: b'I' = idle (outside a txn), b'T' = in a transaction block.
    """
    cc = b"C" + struct.pack("!I", 4 + len(tag) + 1) + tag + b"\x00"
    rfq = b"Z" + struct.pack("!I", 5) + status
    with lock:
        client.sendall(cc + rfq)


def read_startup(client: socket.socket) -> bytes | None:
    """Read the client's StartupMessage, answering any leading SSL/GSS request
    with 'N' (no encryption) so a default-`sslmode=prefer` client falls back to
    plaintext on the localhost hop. Returns the raw StartupMessage bytes."""
    while True:
        hdr = recv_exact(client, 4)
        if hdr is None:
            return None
        (length,) = struct.unpack("!I", hdr)
        body = recv_exact(client, length - 4)
        if body is None:
            return None
        if length == 8:
            (code,) = struct.unpack("!I", body)
            if code in (SSL_REQUEST_CODE, GSS_ENC_REQUEST_CODE):
                client.sendall(b"N")  # not encrypted; client retries in plaintext
                continue
        return hdr + body


def connect_upstream(host: str, port: int) -> ssl.SSLSocket:
    """Open a TLS connection to DSQL (which requires SSL)."""
    upstream = socket.create_connection((host, port))
    upstream.sendall(struct.pack("!II", 8, SSL_REQUEST_CODE))
    resp = upstream.recv(1)
    if resp != b"S":
        upstream.close()
        raise ConnectionError(f"DSQL refused SSL (replied {resp!r})")
    ctx = ssl.create_default_context()
    return ctx.wrap_socket(upstream, server_hostname=host)


def half_close(sock: socket.socket) -> None:
    """Shut down `sock` so a peer thread blocked in `recv` wakes. Best-effort."""
    try:
        sock.shutdown(socket.SHUT_RDWR)
    except OSError:
        pass


def client_to_server(client: socket.socket, server: socket.socket,
                     client_lock: threading.Lock) -> None:
    """Forward client -> server, intercepting the setup statements DSQL rejects.

    Runs after the StartupMessage has been forwarded, so every message here is
    the typed form (1-byte type + Int32 length + body). On exit, closes `server`
    so the paired `server_to_client` thread doesn't hang on `upstream.recv()`."""
    try:
        _pump_client_to_server(client, server, client_lock)
    finally:
        half_close(server)


def _pump_client_to_server(client: socket.socket, server: socket.socket,
                           client_lock: threading.Lock) -> None:
    prepared_queries: dict[bytes, bytes] = {}
    while True:
        type_byte = recv_exact(client, 1)
        if type_byte is None:
            return
        len_bytes = recv_exact(client, 4)
        if len_bytes is None:
            return
        (length,) = struct.unpack("!I", len_bytes)
        body = recv_exact(client, length - 4) if length > 4 else b""
        if body is None:
            return

        if type_byte == b"Q":  # simple query
            query = body[:-1] if body.endswith(b"\x00") else body
            prepared = parse_pgdump_prepare(query)
            if prepared is not None:
                name, select = prepared
                prepared_queries[name] = select
                sys.stderr.write(
                    f"[proxy] retained pg_dump PREPARE {name.decode('ascii')}\n")
                # pg_dump creates these after BEGIN, so the connection remains
                # in its transaction while DSQL sees no PREPARE statement.
                send_command_complete(client, client_lock, b"PREPARE", b"T")
                continue
            execution = rewrite_pgdump_execute(query, prepared_queries)
            if execution is not None:
                name, select = execution
                sys.stderr.write(
                    f"[proxy] expanded pg_dump EXECUTE {name.decode('ascii')}\n")
                server.sendall(frame(b"Q", select + b";\x00"))
                continue
            m = SET_RE.match(query)
            if m and not set_param_allowed(m.group(1)):
                sys.stderr.write(f"[proxy] swallowed SET {m.group(1).decode()}\n")
                send_command_complete(client, client_lock, b"SET", b"I")
                continue
            if LOCK_RE.match(query):
                sys.stderr.write("[proxy] synthesized LOCK TABLE ok\n")
                send_command_complete(client, client_lock, b"LOCK TABLE", b"T")
                continue
            if SET_CONFIG_RE.match(query):
                sys.stderr.write("[proxy] neutralized set_config() probe\n")
                # Forward a rewrite (not the original) — unlike SET/LOCK we want
                # a real server reply. `::text` matches set_config's return type,
                # so a client reading the column type sees text, not `unknown`.
                # Each setup set_config is its own simple query, so replacing the
                # whole body is safe.
                server.sendall(frame(b"Q", b"SELECT NULL::text;\x00"))
                continue
        server.sendall(type_byte + len_bytes + body)


def server_to_client(server: socket.socket, client: socket.socket,
                    client_lock: threading.Lock) -> None:
    """Forward server -> client verbatim. On exit, closes `client` so an upstream
    drop (e.g. DSQL's ~1 h connection cap firing mid-dump) reaches pg_dump as a
    lost connection — a non-zero exit, not a clean EOF that looks like a complete
    dump — and the paired client->server thread doesn't hang."""
    try:
        while True:
            data = server.recv(65536)
            if not data:
                return
            with client_lock:
                client.sendall(data)
    finally:
        half_close(client)


def handle(client: socket.socket, target_host: str, target_port: int) -> None:
    try:
        startup = read_startup(client)
        if startup is None:
            return
        upstream = connect_upstream(target_host, target_port)
        upstream.sendall(startup)

        client_lock = threading.Lock()
        c2s = threading.Thread(
            target=client_to_server, args=(client, upstream, client_lock), daemon=True)
        s2c = threading.Thread(
            target=server_to_client, args=(upstream, client, client_lock), daemon=True)
        c2s.start()
        s2c.start()
        c2s.join()
        s2c.join()
    except Exception as e:  # one bad connection must not take down the proxy
        sys.stderr.write(f"[proxy] connection error: {e}\n")
    finally:
        try:
            client.close()
        except OSError:
            pass


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Wire proxy that lets stock pg_dump/psql read an Aurora DSQL cluster.")
    parser.add_argument("endpoint", help="DSQL cluster endpoint hostname")
    parser.add_argument("--target-port", type=int, default=5432,
                        help="DSQL port (default: 5432)")
    parser.add_argument("--listen-host", default="127.0.0.1",
                        help="local address to listen on (default: 127.0.0.1)")
    parser.add_argument("--listen-port", type=int, default=6543,
                        help="local port to listen on (default: 6543)")
    args = parser.parse_args()

    # The client->proxy hop is plaintext (the proxy answers SSLRequest with 'N'),
    # so the DSQL auth token and dump data cross it unencrypted. Safe on loopback;
    # warn loudly if bound anywhere reachable off-host.
    if args.listen_host not in ("127.0.0.1", "::1", "localhost"):
        sys.stderr.write(
            f"[proxy] WARNING: listening on non-loopback {args.listen_host}; the "
            "DSQL auth token and dump data will traverse the network UNENCRYPTED\n")

    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind((args.listen_host, args.listen_port))
    srv.listen(16)
    sys.stderr.write(
        f"[proxy] listening {args.listen_host}:{args.listen_port} "
        f"-> {args.endpoint}:{args.target_port}\n")
    try:
        while True:
            client, _ = srv.accept()
            threading.Thread(
                target=handle, args=(client, args.endpoint, args.target_port),
                daemon=True).start()
    except KeyboardInterrupt:
        sys.stderr.write("\n[proxy] shutting down\n")
    finally:
        srv.close()


def _self_test() -> None:
    """Offline parser, rewrite, framing, and bidirectional socket checks.

    Run with `python3 dsql_pgdump_proxy.py --self-test`.
    """
    if not __debug__:
        raise SystemExit(
            "self-test requires assertions; run Python without -O/PYTHONOPTIMIZE")

    # SET classification: rejected params are swallowed, content params pass.
    assert not set_param_allowed(b"synchronize_seqscans")
    assert not set_param_allowed(b"statement_timeout")
    assert not set_param_allowed(b"standard_conforming_strings")
    assert set_param_allowed(b"client_encoding")
    assert set_param_allowed(b"search_path")
    assert set_param_allowed(b"enable_seqscan")
    assert set_param_allowed(b"disable_sync_create_index")
    # Statement-shape regexes.
    assert SET_RE.match(b"SET statement_timeout = 0").group(1) == b"statement_timeout"
    assert SET_RE.match(b"set session row_security = off").group(1) == b"row_security"
    assert SET_RE.match(b"SELECT 1") is None
    assert LOCK_RE.match(b"LOCK TABLE public.t IN ACCESS SHARE MODE")
    assert LOCK_RE.match(b"  lock table t")
    assert LOCK_RE.match(b"SELECT 1") is None
    # set_config probe: both real pg_dump forms (the search_path literal and the
    # restrict_nonsystem_relation_kind pg_settings lookup) are neutralized; a
    # `set_config(` substring inside a string literal or a column ref is not.
    assert SET_CONFIG_RE.match(b"SELECT pg_catalog.set_config('search_path', '', false);")
    assert SET_CONFIG_RE.match(
        b"SELECT set_config(name, 'view, foreign-table', false) FROM pg_settings "
        b"WHERE name = 'restrict_nonsystem_relation_kind'")
    assert SET_CONFIG_RE.match(b"SELECT 1") is None
    assert SET_CONFIG_RE.match(b"SELECT * FROM t WHERE c = 'set_config('") is None
    assert SET_CONFIG_RE.match(b"SELECT a, set_config FROM t") is None

    # pg_dump's SQL-level prepared catalog queries are retained and expanded,
    # while arbitrary PREPARE/EXECUTE statements remain untouched for DSQL to
    # reject normally.
    dump_func = (
        b"PREPARE dumpFunc(pg_catalog.oid) AS\n"
        b"SELECT '$1; literal' AS marker, prosrc "
        b"FROM pg_catalog.pg_proc p WHERE p.oid = $1 /* $2; ignored */"
    )
    name, select = parse_pgdump_prepare(dump_func)
    assert name == b"dumpfunc"
    assert select == (
        b"SELECT '$1; literal' AS marker, prosrc "
        b"FROM pg_catalog.pg_proc p WHERE p.oid = $1 /* $2; ignored */"
    )
    prepared_queries = {name: select}
    execute_name, rewritten = rewrite_pgdump_execute(
        b"EXECUTE dumpFunc('12345')", prepared_queries)
    assert execute_name == b"dumpfunc"
    assert rewritten == (
        b"SELECT '$1; literal' AS marker, prosrc FROM pg_catalog.pg_proc p "
        b"WHERE p.oid = ('12345'::pg_catalog.oid) /* $2; ignored */"
    )

    # Every PostgreSQL 16 pg_dump one-OID query name is associated with its
    # expected primary system catalog. These compact fixtures test the complete
    # allowlist without copying large, version-specific queries into this file.
    for query_name, catalog in PGDUMP_OID_PREPARED_QUERIES.items():
        fixture = (
            b"PREPARE " + query_name + b"(pg_catalog.oid) AS "
            b"SELECT 1 FROM pg_catalog." + catalog + b" WHERE oid = $1;"
        )
        parsed = parse_pgdump_prepare(fixture)
        assert parsed is not None
        assert parsed[0] == query_name

    assert parse_pgdump_prepare(
        b"PREPARE userQuery(pg_catalog.oid) AS SELECT $1") is None
    assert parse_pgdump_prepare(
        b"PREPARE dumpFunc(text) AS SELECT $1") is None
    assert parse_pgdump_prepare(
        b"PREPARE dumpFunc(pg_catalog.oid) AS "
        b"SELECT $2 FROM pg_catalog.pg_proc") is None
    assert parse_pgdump_prepare(
        b"PREPARE dumpFunc(pg_catalog.oid) AS "
        b"SELECT $1, $1 FROM pg_catalog.pg_proc") is None
    assert parse_pgdump_prepare(
        b"PREPARE dumpFunc(pg_catalog.oid) AS "
        b"SELECT $1suffix FROM pg_catalog.pg_proc") is None
    assert parse_pgdump_prepare(
        b"PREPARE dumpFunc(pg_catalog.oid) AS "
        b"SELECT $1 FROM pg_catalog.pg_proc; SELECT 2") is None
    assert parse_pgdump_prepare(
        b"PREPARE dumpFunc(pg_catalog.oid) AS "
        b"SELECT $1 FROM pg_catalog.pg_class") is None
    assert parse_pgdump_prepare(
        b"PREPARE dumpFunc(pg_catalog.oid) AS "
        b"SELECT $1 /* FROM pg_catalog.pg_proc */ "
        b"FROM pg_catalog.pg_class") is None
    assert parse_pgdump_prepare(
        b"PREPARE dumpFunc(pg_catalog.oid) AS "
        b"SELECT $$ $1; $$ FROM pg_catalog.pg_proc WHERE oid = $1") is not None
    assert parse_pgdump_prepare(
        b"PREPARE dumpFunc(pg_catalog.oid) AS "
        b"SELECT 'unclosed FROM pg_catalog.pg_proc WHERE oid = $1") is None
    # PostgreSQL 18 statistics export uses a different name[]/name[] query and
    # is deliberately outside this one-OID compatibility path.
    assert parse_pgdump_prepare(
        b"PREPARE getAttributeStats(pg_catalog.name[], pg_catalog.name[]) "
        b"AS SELECT $1") is None
    assert rewrite_pgdump_execute(
        b"EXECUTE userQuery('12345')", prepared_queries) is None
    assert rewrite_pgdump_execute(
        b"EXECUTE dumpFunc('4294967295')", prepared_queries) is not None
    assert rewrite_pgdump_execute(
        b"EXECUTE dumpFunc('4294967296')", prepared_queries) is None
    assert rewrite_pgdump_execute(
        b"EXECUTE dumpFunc('999999999999999999999999')",
        prepared_queries,
    ) is None

    # Exercise both pumps: PREPARE is acknowledged locally, EXECUTE becomes a
    # SELECT upstream, server replies return to the client, and an unfamiliar
    # query passes through byte-for-byte. Timeouts turn protocol regressions
    # into bounded test failures rather than hanging CI.
    client, proxy_client = socket.socketpair()
    proxy_server, upstream = socket.socketpair()
    test_sockets = (client, proxy_client, proxy_server, upstream)
    for sock in test_sockets:
        sock.settimeout(1)

    thread_errors: list[BaseException] = []

    def run_pump(target, *args) -> None:
        try:
            target(*args)
        except BaseException as exc:
            thread_errors.append(exc)

    client_lock = threading.Lock()
    pumps = [
        threading.Thread(
            target=run_pump,
            args=(client_to_server, proxy_client, proxy_server, client_lock),
            daemon=True,
        ),
        threading.Thread(
            target=run_pump,
            args=(server_to_client, proxy_server, proxy_client, client_lock),
            daemon=True,
        ),
    ]
    for pump in pumps:
        pump.start()
    try:
        client.sendall(frame(b"Q", dump_func + b"\x00"))
        prepare_reply = frame(b"C", b"PREPARE\x00") + frame(b"Z", b"T")
        assert recv_exact(client, len(prepare_reply)) == prepare_reply

        client.sendall(frame(b"Q", b"EXECUTE dumpFunc('12345')\x00"))
        expected_select = frame(b"Q", rewritten + b";\x00")
        assert recv_exact(upstream, len(expected_select)) == expected_select

        upstream_reply = frame(b"C", b"SELECT 1\x00") + frame(b"Z", b"T")
        upstream.sendall(upstream_reply)
        assert recv_exact(client, len(upstream_reply)) == upstream_reply

        unknown_query = frame(b"Q", b"SELECT 42;\x00")
        client.sendall(unknown_query)
        assert recv_exact(upstream, len(unknown_query)) == unknown_query

        # Finish through the same half-close path used by a real client. The
        # client pump then closes the proxy's server side, which wakes the
        # server pump without racing descriptor close against recv().
        client.shutdown(socket.SHUT_WR)
        for pump in pumps:
            pump.join(timeout=1)
        assert not thread_errors, thread_errors
        assert all(not pump.is_alive() for pump in pumps)
    finally:
        for sock in test_sockets:
            half_close(sock)
        for pump in pumps:
            pump.join(timeout=1)
        for sock in test_sockets:
            try:
                sock.close()
            except OSError:
                pass

    # frame() length prefix (Int32 covers itself + body) — the set_config path.
    msg = frame(b"Q", b"SELECT NULL::text;\x00")
    assert struct.unpack("!I", msg[1:5])[0] == len(msg) - 1
    assert msg[5:] == b"SELECT NULL::text;\x00"

    # half_close must tolerate an already-closed socket.
    a, b = socket.socketpair()
    a.close()
    b.close()
    half_close(a)
    print("self-test: ok")


if __name__ == "__main__":
    if "--self-test" in sys.argv:
        _self_test()
    else:
        main()
