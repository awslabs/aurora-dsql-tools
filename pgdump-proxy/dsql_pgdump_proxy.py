#!/usr/bin/env python3
"""Let stock `pg_dump` / `psql` read an Aurora DSQL cluster.

Aurora DSQL speaks the PostgreSQL wire protocol but supports only a fixed
allowlist of session parameters and a subset of statements. `pg_dump` issues a
handful of connection-setup statements that DSQL rejects ("setting configuration
parameter X not supported" / "unsupported statement: Lock"), and pg_dump aborts
on the first error — so you cannot dump a DSQL cluster with stock tooling.

This proxy sits between the client (plaintext, on localhost) and DSQL (TLS). It
passes the startup and authentication bytes through untouched — DSQL's IAM token
is just a cleartext password, so the proxy never terminates auth — and only
intercepts the specific setup statements DSQL rejects, none of which affect dump
*content*:

  * `SET <param>` for a param DSQL rejects        -> synth a `SET` success reply
  * `SELECT ... set_config(...)` setup probe       -> rewrite to `SELECT NULL::text`
  * `LOCK TABLE ... IN ... MODE`                  -> synth a `LOCK TABLE` reply
                                                     (DSQL is snapshot-isolated;
                                                      the lock is unnecessary)

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

SSL_REQUEST_CODE = 80877103
GSS_ENC_REQUEST_CODE = 80877104


def set_param_allowed(param: bytes) -> bool:
    p = param.decode("ascii", "replace").lower()
    return (
        p in ALLOWED_SET_PARAMS
        or p.startswith("enable_")
        or p == "disable_sync_create_index"
    )


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
            m = SET_RE.match(body)
            if m and not set_param_allowed(m.group(1)):
                sys.stderr.write(f"[proxy] swallowed SET {m.group(1).decode()}\n")
                send_command_complete(client, client_lock, b"SET", b"I")
                continue
            if LOCK_RE.match(body):
                sys.stderr.write("[proxy] synthesized LOCK TABLE ok\n")
                send_command_complete(client, client_lock, b"LOCK TABLE", b"T")
                continue
            if SET_CONFIG_RE.match(body):
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
    """Offline checks for the statement-classification logic (the only
    non-trivial part). Run with `python3 dsql_pgdump_proxy.py --self-test`."""
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
