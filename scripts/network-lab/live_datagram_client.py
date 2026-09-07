"""Exercise the owned Controller datagram contract, without OS capture claims."""

import json
import os
import socket
import struct
import sys
import time
from pathlib import Path

gate, output, hostname, case, api_port, mode = sys.argv[1:]
deadline = time.monotonic() + 90
while not Path(gate).exists():
    if time.monotonic() > deadline:
        raise TimeoutError("datagram gate was never released")
    time.sleep(0.05)
identity = json.loads(Path(gate).read_text())["agent_id"].encode()
result = {"pid": os.getpid(), "case": case, "transport": "owned-framed-udp"}


def exactly(stream, count):
    data = b""
    while len(data) < count:
        part = stream.recv(count - len(data))
        if not part:
            raise EOFError("association closed")
        data += part
    return data


try:
    with socket.create_connection(
        ("127.0.0.1", int(api_port) + 1), timeout=15
    ) as stream:
        stream.sendall(b"\x05\x01\x02")
        assert exactly(stream, 2) == b"\x05\x02"
        stream.sendall(b"\x01" + bytes([len(identity)]) + identity + b"\x05treer")
        assert exactly(stream, 2) == b"\x01\x00"
        name, separator, selected_port = hostname.partition("|")
        host = name.encode()
        stream.sendall(
            b"\x05\xf0\x00\x03"
            + bytes([len(host)])
            + host
            + struct.pack("!H", int(selected_port) if separator else 8080)
        )
        response = exactly(stream, 10)
        if response[1]:
            result.update(passed=case == "deny", policy_denied=True)
        else:
            assert case == "allow", "unauthorized datagram association accepted"
            payload = b"Treer\x00\xffmacOS"
            stream.sendall(struct.pack("!H", len(payload)) + payload)
            reply = exactly(stream, struct.unpack("!H", exactly(stream, 2))[0])
            assert reply == b"GUEST:" + payload[::-1], reply
            result.update(
                passed=True, sent_bytes=len(payload), received_bytes=len(reply)
            )
            if mode == "hold":
                Path(output + ".ready").write_text("connected")
                stream.settimeout(25)
                assert stream.recv(1) == b"", "revocation did not close the association"
                result["active_revocation_verified"] = True
except (OSError, EOFError) as error:
    result.update(error=str(error), passed=False)
Path(output).write_text(json.dumps(result))
print(json.dumps(result), flush=True)
