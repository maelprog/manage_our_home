#!/usr/bin/env python3
"""Raw-socket probes of the ingress for ci/ingress-test.sh (#434): what curl
cannot do — trickle a request's headers, hold a WebSocket open in silence.
Standard library only; TLS to the node's address with the host as SNI, the
certificate not checked (the k3d job's is self-signed, ci/ci-issuer.yaml).

  ingress-probe.py drip <address> <host> <give-up seconds>
      Sends a request's headers one line every 5 s, never ending them.
      Prints the second the ingress cut the connection, or exits 1 if it
      was still open at <give-up seconds>.

  ingress-probe.py post <address> <host> <path> <length> <give-up seconds> [method]
      POSTs (or sends with [method]) a body of <length> bytes at 2 kB/s, over HTTP/1.1. Prints the
      status line of the answer and the second it came, or exits 1 if
      none came by <give-up seconds>.

  ingress-probe.py ws <address> <host> <path> <cookie> <marker> <timeout>
      Opens a WebSocket on <path> with the session cookie <cookie>, prints
      `open`, then waits up to <timeout> seconds for a text frame that
      contains <marker>. Exits 0 when it comes, 1 on a close, an error or
      the timeout.
"""

import base64
import os
import socket
import ssl
import sys
import time


def connect(address, host):
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    # HTTP/1.1: a browser's WebSocket upgrade, and the protocol Go reads a
    # request's headers in one at a time.
    ctx.set_alpn_protocols(["http/1.1"])
    raw = socket.create_connection((address, 443), timeout=10)
    return ctx.wrap_socket(raw, server_hostname=host)


def drip(address, host, give_up):
    s = connect(address, host)
    start = time.monotonic()
    s.sendall(f"GET / HTTP/1.1\r\nHost: {host}\r\n".encode())
    s.settimeout(5)
    while time.monotonic() - start < give_up:
        try:
            s.sendall(b"X-Drip: a\r\n")
            data = s.recv(4096)
        except socket.timeout:
            continue
        except (OSError, ssl.SSLError):
            data = b""
        # Whatever came back (nothing, or an error status) ended the
        # request before its headers did.
        print(f"cut after {time.monotonic() - start:.0f} s: {data[:60]!r}", flush=True)
        return 0
    print(f"still open after {give_up} s", flush=True)
    return 1


def post(address, host, path, length, give_up, method="POST"):
    """POST with a body of <length> bytes announced, sent at 2 kB/s over
    HTTP/1.1; prints the status of the answer and when it came, or exits
    1 if none came in <give_up> seconds."""
    s = connect(address, host)
    start = time.monotonic()
    s.sendall(
        (
            f"{method} {path} HTTP/1.1\r\nHost: {host}\r\n"
            f"Content-Type: application/octet-stream\r\nContent-Length: {length}\r\n\r\n"
        ).encode()
    )
    s.settimeout(0.1)
    sent = 0
    while time.monotonic() - start < give_up:
        if sent < length:
            chunk = min(200, length - sent)
            try:
                s.sendall(b"a" * chunk)
                sent += chunk
            except (OSError, ssl.SSLError):
                pass
        try:
            data = s.recv(4096)
        except socket.timeout:
            continue
        except (OSError, ssl.SSLError) as e:
            print(f"connection lost after {time.monotonic() - start:.1f} s: {e!r}", flush=True)
            return 1
        status = data.split(b"\r\n", 1)[0].decode(errors="replace")
        print(f"{status} after {time.monotonic() - start:.1f} s, {sent} bytes sent", flush=True)
        return 0
    print(f"no answer after {give_up} s, {sent} bytes sent", flush=True)
    return 1


def ws(address, host, path, cookie, marker, timeout):
    s = connect(address, host)
    key = base64.b64encode(os.urandom(16)).decode()
    s.sendall(
        (
            f"GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\n"
            f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
            f"Sec-WebSocket-Version: 13\r\nCookie: {cookie}\r\n\r\n"
        ).encode()
    )
    buf = b""
    while b"\r\n\r\n" not in buf:
        chunk = s.recv(4096)
        if not chunk:
            print("closed during the upgrade", flush=True)
            return 1
        buf += chunk
    head, buf = buf.split(b"\r\n\r\n", 1)
    status = head.split(b"\r\n", 1)[0]
    if b" 101 " not in status + b" ":
        print(f"upgrade refused: {head.decode(errors='replace')}", flush=True)
        return 1
    print("open", flush=True)
    start = time.monotonic()
    s.settimeout(timeout)

    def read(n):
        nonlocal buf
        while len(buf) < n:
            chunk = s.recv(65536)
            if not chunk:
                raise EOFError
            buf += chunk
        out, buf = buf[:n], buf[n:]
        return out

    try:
        while True:
            b0, b1 = read(2)
            opcode, n = b0 & 0x0F, b1 & 0x7F
            if n == 126:
                n = int.from_bytes(read(2), "big")
            elif n == 127:
                n = int.from_bytes(read(8), "big")
            payload = read(n)  # a server's frames are not masked
            if opcode == 0x8:
                print(f"closed by the server after {time.monotonic() - start:.0f} s: {payload!r}", flush=True)
                return 1
            if opcode == 0x1 and marker.encode() in payload:
                print(f"message after {time.monotonic() - start:.0f} s", flush=True)
                return 0
    except (EOFError, OSError, ssl.SSLError) as e:
        print(f"connection lost after {time.monotonic() - start:.0f} s: {e!r}", flush=True)
        return 1


if __name__ == "__main__":
    cmd, args = sys.argv[1], sys.argv[2:]
    if cmd == "drip":
        sys.exit(drip(args[0], args[1], int(args[2])))
    if cmd == "post":
        sys.exit(post(args[0], args[1], args[2], int(args[3]), int(args[4]), *args[5:6]))
    if cmd == "ws":
        sys.exit(ws(args[0], args[1], args[2], args[3], args[4], int(args[5])))
    sys.exit(f"unknown probe: {cmd}")
