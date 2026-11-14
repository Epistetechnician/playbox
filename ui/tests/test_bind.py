#!/usr/bin/env python3
"""The hub must start answering without a reverse-DNS lookup: getfqdn is made to hang, and the server still binds at once."""
import os
import socket
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
from hub.server import QuickBindServer  # noqa: E402

# REQ: HUB-1


def main() -> int:
    real = socket.getfqdn

    def hang(*_a, **_k):
        time.sleep(30)
        return "never"

    socket.getfqdn = hang
    try:
        t0 = time.time()
        import http.server

        srv = QuickBindServer(("127.0.0.1", 0), http.server.BaseHTTPRequestHandler)
        took = time.time() - t0
        srv.server_close()
    finally:
        socket.getfqdn = real
    ok = took < 2 and srv.server_name == "127.0.0.1"
    print(("PASS" if ok else "FAIL") + f" binds without a reverse-DNS lookup ({took:.2f}s)")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
