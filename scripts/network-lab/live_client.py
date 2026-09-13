"""Plain-socket workload launched by a real Treer Host for the opt-in lab."""

import json
import os
import socket
import sys
import time
from pathlib import Path

from transport import PROXY_ENV

gate, output, hostname, case, api_port, half_close = sys.argv[1:]
for key in PROXY_ENV:
    os.environ.pop(key, None)
deadline = time.monotonic() + 90
while not Path(gate).exists():
    if time.monotonic() > deadline:
        raise TimeoutError("capture registration gate was never released")
    time.sleep(0.05)
result = {
    "pid": os.getpid(),
    "case": case,
    "proxy_env": any(key in os.environ for key in PROXY_ENV),
}
payload = b"Treer\x00\xffmacOS"
try:
    with socket.create_connection((hostname, 8080), timeout=10) as stream:
        stream.sendall(payload)
        if half_close == "yes":
            stream.shutdown(socket.SHUT_WR)
        reply = b""
        while data := stream.recv(4096):
            reply += data
            if half_close == "hold" and len(reply) == 18:
                Path(output + ".ready").write_text("connected")
                stream.settimeout(25)
        if half_close == "hold":
            result["active_revocation_verified"] = len(reply) == 18
    result.update(
        reply_hex=reply.hex(), sent_bytes=len(payload), received_bytes=len(reply)
    )
    result["passed"] = (
        (reply == b"GUEST:" + payload[::-1]) if case == "allow" else not reply
    )
except OSError as error:
    result.update(error=str(error), passed=case == "deny")
if case == "allow":
    with socket.create_connection(("192.0.2.1", int(api_port)), timeout=10) as stream:
        stream.sendall(
            b"GET /api/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        response = b""
        while data := stream.recv(4096):
            response += data
    result["local_api_passed"] = b"200 OK" in response
    result["passed"] &= result["local_api_passed"]
Path(output).write_text(json.dumps(result))
print(json.dumps(result), flush=True)
