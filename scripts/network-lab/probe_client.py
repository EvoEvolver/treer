#!/usr/bin/env python3
"""Ordinary socket/curl client. Has no knowledge of SOCKS or the capture backend."""

import os
import socket
import sys

sys.stdin.buffer.read(1)
host, case = sys.argv[1:]
if case == "tls":
    # Preserve PID across exec for the native per-PID capture test.
    os.execv(
        "/usr/bin/curl",
        ["curl", "--noproxy", "*", "-fsS", "--max-time", "20", "https://example.com"],
    )
try:
    with socket.create_connection((host, 8080), timeout=8) as connection:
        connection.sendall(
            b"GET / HTTP/1.1\r\nHost: probe\r\nConnection: close\r\n\r\n"
        )
        data = connection.recv(4096)
        print(data.decode() if data else "CONNECT_REJECTED EOF", flush=True)
except OSError as error:
    print("CONNECT_REJECTED", type(error).__name__, flush=True)
