#!/usr/bin/env python3
"""Check two actual Linux sandboxes can share private loopback port 18761.
Also exercise Controller-style Unix service bridges and host-loopback publishing.
Uses an isolated SOCKS fixture; does not change enrolled machines or services.
"""

import argparse
import asyncio
import contextlib
import json
import os
import struct
import sys
import tempfile
from pathlib import Path

from capture_probe import PROXY_ENV, Fixture


async def main(args):
    fixture = Fixture()
    socks = await asyncio.start_server(fixture.handle, "127.0.0.1", 0)
    socks_port = socks.sockets[0].getsockname()[1]
    reserve = await asyncio.start_server(lambda r, w: w.close(), "127.0.0.1", 0)
    published_port = reserve.sockets[0].getsockname()[1]
    reserve.close()
    await reserve.wait_closed()
    processes = []
    result = {
        "platform": sys.platform,
        "proof_scope": "actual Linux sandbox service bridge and publish",
        "cases": [],
    }
    env = {k: v for k, v in os.environ.items() if k not in PROXY_ENV}
    try:
        with tempfile.TemporaryDirectory(prefix="treer-services-") as tmp:
            child = Path(tmp) / "servers.py"
            child.write_text(
                """import asyncio,sys\nasync def main():\n async def serve(r,w):\n  await r.read(4); w.write(sys.argv[1].encode()); await w.drain(); w.close()\n for port in (18761,int(sys.argv[2])):\n  await asyncio.start_server(serve,"127.0.0.1",port)\n print("READY",flush=True)\n await asyncio.Event().wait()\nasyncio.run(main())\n"""
            )
            paths = []
            for index in range(2):
                path = f"{tmp}/agent-{index}.sock"
                paths.append(path)
                cmd = [
                    args.binary,
                    "sandbox-exec",
                    "--network-proxy",
                    f"socks5://services_{index}:treer@127.0.0.1:{socks_port}",
                    "--service-socket",
                    path,
                ]
                if index == 0:
                    cmd += ["--publish", str(published_port)]
                cmd += [
                    "--",
                    sys.executable,
                    str(child),
                    f"AGENT_{index}",
                    str(published_port),
                ]
                proc = await asyncio.create_subprocess_exec(
                    *cmd,
                    env=env,
                    stdout=asyncio.subprocess.PIPE,
                    stderr=asyncio.subprocess.PIPE,
                    start_new_session=True,
                )
                processes.append(proc)
                ready = await asyncio.wait_for(proc.stdout.readline(), 10)
                if ready.strip() != b"READY":
                    raise RuntimeError(
                        f"sandbox startup failed: {(await proc.stderr.read()).decode()}"
                    )
            for index, path in enumerate(paths):
                r, w = await asyncio.open_unix_connection(path)
                w.write(struct.pack("!H", 18761))
                await w.drain()
                assert await r.readexactly(1) == b"\0"
                w.write(b"ping")
                await w.drain()
                data = await asyncio.wait_for(r.read(100), 5)
                w.close()
                result["cases"].append(
                    {
                        "name": f"private_loopback_agent_{index}",
                        "passed": data == f"AGENT_{index}".encode(),
                        "reply": data.decode(),
                    }
                )
            r, w = await asyncio.open_connection("127.0.0.1", published_port)
            w.write(b"ping")
            await w.drain()
            data = await asyncio.wait_for(r.readexactly(7), 5)
            w.write_eof()
            w.close()
            result["cases"].append(
                {
                    "name": "host_loopback_publish",
                    "passed": data == b"AGENT_0",
                    "reply": data.decode(),
                }
            )
    finally:
        import signal

        for proc in processes:
            if proc.returncode is None:
                with contextlib.suppress(ProcessLookupError):
                    os.killpg(proc.pid, signal.SIGTERM)
        for proc in processes:
            with contextlib.suppress(asyncio.TimeoutError):
                await asyncio.wait_for(proc.communicate(), 5)
            if proc.returncode is None:
                with contextlib.suppress(ProcessLookupError):
                    os.killpg(proc.pid, signal.SIGKILL)
                await proc.communicate()
        socks.close()
        await socks.wait_closed()
    result["passed"] = all(c["passed"] for c in result["cases"])
    print(json.dumps(result, indent=2))
    if args.output:
        Path(args.output).write_text(json.dumps(result, indent=2) + "\n")
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", required=True)
    p.add_argument("--output")
    raise SystemExit(asyncio.run(main(p.parse_args())))
