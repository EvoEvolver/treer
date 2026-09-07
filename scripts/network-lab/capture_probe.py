#!/usr/bin/env python3
"""Reproducible capture-layer probe. No enrollment or shared Policy changes.

Linux: python3 capture_probe.py --linux-binary /opt/treer/bin/treer-agent-server
macOS: python3 capture_probe.py --macos (requires mitmproxy_rs and OS approval)
The SOCKS fixture deliberately labels its policy and counters as fixture evidence.
"""

import argparse
import asyncio
import ipaddress
import json
import os
import struct
import sys
import tempfile
from pathlib import Path

from transport import PROXY_ENV, splice


class Fixture:
    def __init__(self):
        self.records = []
        self.resolver = None

    async def handle(self, r, w):
        record = None
        try:
            v, n = await r.readexactly(2)
            assert v == 5 and 2 in await r.readexactly(n)
            w.write(b"\x05\x02")
            await w.drain()
            assert await r.readexactly(1) == b"\x01"
            agent = (await r.readexactly((await r.readexactly(1))[0])).decode()
            password = await r.readexactly((await r.readexactly(1))[0])
            assert password == b"treer"
            w.write(b"\x01\x00")
            await w.drain()
            v, cmd, _, atyp = await r.readexactly(4)
            assert (v, cmd) == (5, 1)
            if atyp == 3:
                host = (await r.readexactly((await r.readexactly(1))[0])).decode()
            else:
                host = str(
                    ipaddress.ip_address(await r.readexactly(4 if atyp == 1 else 16))
                )
            port = struct.unpack("!H", await r.readexactly(2))[0]
            deny = host in ("deny.treer.invalid", "198.18.42.2")
            record = {
                "agent_id": agent,
                "host": host,
                "port": port,
                "fixture_policy": "deny" if deny else "allow",
            }
            self.records.append(record)
            if deny:
                w.write(b"\x05\x02\x00\x01" + b"\0" * 6)
                await w.drain()
                return
            w.write(b"\x05\x00\x00\x01" + b"\0" * 6)
            await w.drain()
            if host in ("echo.treer.invalid", "198.18.42.1", "192.0.2.1"):
                data = await r.read(65536)
                record["sent_bytes"] = len(data)
                if data.startswith(b"GET "):
                    body = b"TREER_CAPTURE_OK\n"
                    reply = (
                        b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: "
                        + str(len(body)).encode()
                        + b"\r\n\r\n"
                        + body
                    )
                else:
                    reply = data
                w.write(reply)
                await w.drain()
                record["received_bytes"] = len(reply)
            else:
                address = host
                if self.resolver:
                    address = (await self.resolver.lookup_ipv4(host))[0]
                upstream_r, upstream_w = await asyncio.open_connection(address, port)
                sent, received = await splice(r, w, upstream_r, upstream_w)
                record.update(sent_bytes=sent, received_bytes=received)
        except (OSError, asyncio.IncompleteReadError) as e:
            if record is not None:
                record["error"] = str(e)
        finally:
            w.close()


async def main(args):
    fixture = Fixture()
    server = await asyncio.start_server(fixture.handle, "127.0.0.1", 0)
    port = server.sockets[0].getsockname()[1]
    env = {k: v for k, v in os.environ.items() if k not in PROXY_ENV}
    result = {
        "platform": sys.platform,
        "proof_scope": "capture + SOCKS fixture (not live Treer Policy or ledger)",
        "cases": [],
    }
    redirector = None
    selected = {}
    if args.macos:
        import mitmproxy_rs
        from macos_capture import MacCapture

        fixture.resolver = mitmproxy_rs.dns.DnsResolver()
        redirector = MacCapture(port, result)
        print("Starting macOS network extension...", flush=True)
        await redirector.start()
        selected = redirector.selected
    try:
        with tempfile.TemporaryDirectory(prefix="treer-capture-") as tmp:
            # Child waits for stdin before connecting so interception is configured first.
            child = Path(__file__).with_name("probe_client.py").resolve()
            cases = [
                ("literal", "198.18.42.1"),
                ("deny", "198.18.42.2"),
                ("local_api", "192.0.2.1"),
            ]
            cases += [
                ("virtual_dns", "echo.treer.invalid"),
                ("virtual_deny", "deny.treer.invalid"),
            ]
            cases.append(("tls", "example.com"))
            for index, (case, host) in enumerate(cases):
                agent = f"probe_agent_{index}"
                command = [sys.executable, str(child), host, case]
                if args.linux_binary:
                    command = [
                        args.linux_binary,
                        "sandbox-exec",
                        "--network-proxy",
                        f"socks5://{agent}:treer@127.0.0.1:{port}",
                        "--service-socket",
                        f"{tmp}/service-{index}.sock",
                        "--",
                        *command,
                    ]
                proc = await asyncio.create_subprocess_exec(
                    *command,
                    env=env,
                    stdin=asyncio.subprocess.PIPE,
                    stdout=asyncio.subprocess.PIPE,
                    stderr=asyncio.subprocess.PIPE,
                )
                if redirector:
                    selected[proc.pid] = agent
                    redirector.set_intercept(",".join(str(p) for p in selected))
                    # Signed upstream API has no acknowledged configuration barrier.
                    await asyncio.sleep(0.3)
                try:
                    out, err = await asyncio.wait_for(proc.communicate(b"x"), 35)
                except asyncio.TimeoutError:
                    proc.kill()
                    out, err = await proc.communicate()
                expected = (
                    b"CONNECT_REJECTED"
                    if "deny" in case
                    else b"Example Domain"
                    if case == "tls"
                    else b"TREER_CAPTURE_OK"
                )
                evidence = next(
                    (
                        r
                        for r in fixture.records
                        if r["agent_id"] == agent
                        and (r["host"] == host or case == "tls" and r["port"] == 443)
                    ),
                    None,
                )
                passed = (
                    proc.returncode == 0 and expected in out and evidence is not None
                )
                entry = {
                    "name": case,
                    "passed": passed,
                    "exit_code": proc.returncode,
                    "stdout": out.decode(errors="replace")[-1500:],
                    "stderr": err.decode(errors="replace")[-1500:],
                }
                result["cases"].append(entry)
                print(json.dumps(entry), flush=True)
                if redirector:
                    selected.pop(proc.pid, None)
                    redirector.set_intercept(",".join(str(p) for p in selected) or "0")
    finally:
        if redirector:
            await redirector.stop()
        server.close()
        await server.wait_closed()
    result["socks_records"] = fixture.records
    result["passed"] = all(c["passed"] for c in result["cases"])
    print(json.dumps(result, indent=2), flush=True)
    if args.output:
        Path(args.output).write_text(json.dumps(result, indent=2) + "\n")
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    mode = p.add_mutually_exclusive_group(required=True)
    mode.add_argument("--linux-binary")
    mode.add_argument("--macos", action="store_true")
    p.add_argument("--output")
    raise SystemExit(asyncio.run(main(p.parse_args())))
