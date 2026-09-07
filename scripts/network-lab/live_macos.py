#!/usr/bin/env python3
"""Opt-in native capture → two real Treer Proxies/NATS → Apple Linux TCP lab.

Uses a disposable database on the local *test* Postgres container, two loopback
auth-disabled Proxies and two temporary Mac Hosts. No existing enrollment is
read or modified. Native capture is cooperative; this is not a supported mode.
"""

import argparse
import asyncio
import contextlib
import json
import os
import socket
import sys
import time
import urllib.error
import urllib.request
import uuid
from datetime import datetime, timezone
from pathlib import Path

from macos_capture import MacCapture

REPO = Path(__file__).resolve().parents[2]
RESERVED_PORTS = set()


def free_port_pair():
    while True:
        with socket.socket() as first, socket.socket() as second:
            first.bind(("127.0.0.1", 0))
            port = first.getsockname()[1]
            if port in RESERVED_PORTS or port + 1 in RESERVED_PORTS:
                continue
            try:
                second.bind(("127.0.0.1", port + 1))
            except OSError:
                continue
            RESERVED_PORTS.update((port, port + 1))
            return port


async def command(*args, data=None):
    proc = await asyncio.create_subprocess_exec(
        *map(str, args),
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    out, err = await asyncio.wait_for(proc.communicate(data), 30)
    if proc.returncode:
        raise RuntimeError(f"{args[0]} failed: {err.decode()[-1500:]}")
    return out.decode()


async def api(port, path, data=None, method=None):
    def request():
        req = urllib.request.Request(
            f"http://127.0.0.1:{port}{path}",
            data=json.dumps(data).encode() if data is not None else None,
            headers={"Content-Type": "application/json"},
            method=method,
        )
        try:
            with urllib.request.urlopen(req, timeout=10) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            raise RuntimeError(
                f"{path}: {error.code} {error.read().decode()}"
            ) from error

    return await asyncio.to_thread(request)


async def eventually(check, seconds=25):
    deadline = time.monotonic() + seconds
    last = None
    while time.monotonic() < deadline:
        try:
            value = await check()
            if value:
                return value
        except (OSError, RuntimeError) as error:
            last = error
        await asyncio.sleep(0.25)
    raise TimeoutError(f"lab readiness timed out: {last}")


async def main(args):
    token = uuid.uuid4().hex[:10]
    database = f"treer_network_lab_{token}"
    stream_name = f"TREER_NETWORK_LAB_{token}"
    root = REPO / "output/network-research" / f"live-{token}"
    root.mkdir(parents=True)
    os.chmod(root, 0o700)
    processes, logs, agents = [], [], []
    guest = capture = None
    created_database = started_nats = False
    nats_container = f"treer-network-lab-{token}"
    nats_port = free_port_pair()
    result = {
        "proof_scope": "real Mac Hosts/Controllers + two Proxies + NATS + PostgreSQL + Apple Linux TCP target",
        "run_id": token,
        "half_close": args.half_close,
        "cases": [],
        "limitations": [
            "cooperative PID gate; no descendant registration or fail-closed isolation",
            "Controller remains proxy-env; real Policy/ledger proof is for registered virtual hosts",
            "Apple target is host-network TCP reached through the second Mac Controller",
            "two local Proxy instances, not geographic regions",
        ],
    }
    env = {
        k: v
        for k, v in os.environ.items()
        if k in ("PATH", "HOME", "USER", "LOGNAME", "SHELL", "LANG", "TMPDIR")
    }
    env.update(
        TREER_NETWORK_MODE="proxy-env",
        NO_COLOR="1",
        RUST_LOG="info,treer_agent_server::network=debug,treer_proxy::agent_socket=debug",
    )

    async def sql(query, db=database):
        return await command(
            "docker",
            "exec",
            "-i",
            args.postgres_container,
            "psql",
            "-U",
            "treer",
            "-d",
            db,
            "-v",
            "ON_ERROR_STOP=1",
            "-At",
            data=query.encode(),
        )

    async def start(name, argv):
        log = (root / f"{name}.log").open("wb")
        logs.append(log)
        proc = await asyncio.create_subprocess_exec(
            *map(str, argv),
            env=env,
            stdout=log,
            stderr=log,
        )
        processes.append(proc)
        return proc

    ports = [free_port_pair() for _ in range(4)]
    proxy_a, proxy_b, controller_a, controller_b = ports
    try:
        await sql(f'CREATE DATABASE "{database}";', db="postgres")
        created_database = True
        await command(
            "docker",
            "run",
            "--rm",
            "-d",
            "--name",
            nats_container,
            "-p",
            f"127.0.0.1:{nats_port}:4222",
            args.nats_image,
            "-js",
        )
        started_nats = True
        for index, port in enumerate((proxy_a, proxy_b)):
            await start(
                f"proxy-{index}",
                [
                    REPO / "target/debug/treer-proxy",
                    "--disable-auth",
                    "--listen",
                    f"127.0.0.1:{port}",
                    "--public-url",
                    f"http://127.0.0.1:{port}",
                    "--database-url",
                    f"postgres://treer:treer@127.0.0.1:{args.postgres_port}/{database}",
                    "--nats-url",
                    f"nats://127.0.0.1:{nats_port}",
                    "--nats-stream",
                    stream_name,
                    "--nats-subject-prefix",
                    f"treer.lab.{token}.events",
                    "--nats-cluster-subject-prefix",
                    f"treer.lab.{token}.cluster",
                    "--proxy-instance-id",
                    f"lab-proxy-{index}-{token}",
                ],
            )
            await eventually(lambda port=port: api(port, "/api/health"))
        organization = (
            await api(proxy_a, "/api/organizations", {"name": f"Network lab {token}"})
        )["organization"]["organization_id"]
        workspace = (
            await api(
                proxy_a,
                "/api/workspaces",
                {
                    "organization_id": organization,
                    "name": f"Native network lab {token}",
                },
            )
        )["workspace"]["workspace_id"]
        result["workspace_id"] = workspace
        base = f"/api/workspaces/{workspace}"
        for index, (port, proxy) in enumerate(
            ((controller_a, proxy_a), (controller_b, proxy_b))
        ):
            home = root / f"host-{index}"
            home.mkdir()
            # macOS Unix socket paths are limited to 104 bytes.
            host_socket = f"/tmp/treer-lab-{token}-{index}.sock"
            config = {
                "proxy": f"http://127.0.0.1:{proxy}",
                "workspace": workspace,
                "server_id": f"srv_lab_{token}_{index}",
                "machine_token": "lab-disabled-auth",
                "operator_credential": "lab-local-operator",
                "root": str(home),
                "listen": f"127.0.0.1:{port}",
                "host_socket": host_socket,
                "install_hostname": socket.gethostname(),
                "service_manager": "foreground",
            }
            (home / "controller.json").write_text(json.dumps(config))
            (home / "host.json").write_text(
                json.dumps(
                    {
                        "socket_path": host_socket,
                        "controller_path": str(
                            REPO / "target/debug/treer-agent-server"
                        ),
                        "controller_config_path": str(home / "controller.json"),
                        "root": str(home),
                    }
                )
            )
            await start(
                f"host-{index}",
                [
                    REPO / "target/debug/treer-agent-host",
                    "run",
                    "--config",
                    home / "host.json",
                ],
            )
            await eventually(lambda port=port: api(port, "/api/health"))

        async def machines_ready():
            snapshot = await api(proxy_a, base + "/snapshot")
            return snapshot if len(snapshot.get("servers", [])) == 2 else None

        result["bootstrap"] = await eventually(machines_ready)
        print(
            "Two real Proxies and Mac Controllers connected through NATS.", flush=True
        )

        target_ip = "127.0.0.1" if args.local_target else args.guest_ip
        result["target_environment"] = (
            "macos-loopback" if args.local_target else "apple-linux-guest"
        )
        target_command = (
            [sys.executable]
            if args.local_target
            else [
                "container",
                "machine",
                "run",
                "-i",
                "-n",
                args.machine,
                "--",
                "python3",
            ]
        )
        guest = await asyncio.create_subprocess_exec(
            *target_command,
            str(Path(__file__).with_name("guest_echo.py")),
            target_ip,
            "udp"
            if args.datagram
            else "hold"
            if args.revoke_active
            else "yes"
            if args.half_close
            else "no",
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        target = json.loads(await asyncio.wait_for(guest.stdout.readline(), 15))
        # Separate guest reachability failures from Policy/capture failures.
        if not args.datagram:
            _, preflight = await asyncio.wait_for(
                asyncio.open_connection(target_ip, target["port"]), 5
            )
            preflight.close()
            await preflight.wait_closed()
        service = (
            await api(
                proxy_a,
                base + "/services",
                {
                    "name": f"guest-echo-{token}",
                    "server_id": f"srv_lab_{token}_1",
                    "target_host": target_ip,
                    "target_port": target["port"],
                    "protocol": "udp" if args.datagram else "tcp",
                },
            )
        )["service"]
        hostname = f"live-{token}.treer.invalid"
        await api(
            proxy_a,
            base + "/virtual-hosts",
            {"hostname": hostname, "service_id": service["service_id"]},
        )
        result["service"] = service
        if args.direct_datagram:
            hostname = f"{target_ip}|{target['port']}"
        if args.datagram:
            result["proof_scope"] = (
                "owned UDP Controller transport + two Proxies/NATS + PostgreSQL; no native OS capture"
            )
            result["limitations"] = [
                "owned extension is not installed",
                "two local Proxies, not geographic regions",
            ]
        else:
            capture = MacCapture(controller_a + 1, result)
            await capture.start()

        for case in ("allow", "deny"):
            gate, output = root / f"{case}.go", root / f"{case}.json"
            agent = await api(
                proxy_a,
                base + "/agents",
                {
                    "server_id": f"srv_lab_{token}_0",
                    "kind": "command",
                    "name": f"native-{case}",
                    "cwd": ".",
                    "args": [
                        sys.executable,
                        str(
                            Path(__file__).with_name(
                                "live_datagram_client.py"
                                if args.datagram
                                else "live_client.py"
                            )
                        ),
                        str(gate),
                        str(output),
                        hostname,
                        case,
                        str(controller_a),
                        "hold"
                        if args.revoke_active and case == "allow"
                        else "yes"
                        if args.half_close
                        else "no",
                    ],
                },
            )
            agents.append(agent)
            if capture:
                capture.selected[agent["pid"]] = agent["agent_id"]
        result["agents"] = agents
        policy = {
            "schema_version": 1,
            "defaults": {"network.connect": "deny"},
            "groups": {},
            "rules": [
                {
                    "id": "allow-one-native-agent",
                    "priority": 100,
                    "effect": "allow",
                    "subjects": [{"kind": "agent", "id": agents[0]["agent_id"]}],
                    "actions": ["network.connect"],
                    "resources": [{}],
                }
            ],
        }
        # This table belongs only to the disposable DB; no shared Policy is edited.
        encoded = json.dumps(policy).replace("'", "''")
        timestamp = datetime.now(timezone.utc).isoformat()
        await sql(
            f"INSERT INTO workspace_policies VALUES ('{workspace}',1,1,'enforce','{encoded}'::jsonb,'{timestamp}','human','network-lab');"
        )
        if capture:
            capture.set_intercept(",".join(map(str, capture.selected)))
        await asyncio.sleep(
            6
        )  # real Policy cache TTL (5 s), plus probe registration settle.
        for case in ("allow", "deny"):
            (root / f"{case}.go").write_text(
                json.dumps(
                    {"agent_id": agents[0 if case == "allow" else 1]["agent_id"]}
                )
            )
            if args.revoke_active and case == "allow":

                async def connected():
                    return (root / "allow.json.ready").exists()

                await eventually(connected)
                revoked = dict(policy, rules=[])
                encoded_revoked = json.dumps(revoked).replace("'", "''")
                timestamp = datetime.now(timezone.utc).isoformat()
                await sql(
                    f"UPDATE workspace_policies SET revision=2, document='{encoded_revoked}'::jsonb, updated_at='{timestamp}' WHERE workspace_id='{workspace}';"
                )

            async def completed(case=case):
                path = root / f"{case}.json"
                return json.loads(path.read_text()) if path.exists() else None

            entry = await eventually(completed)
            if args.revoke_active and case == "allow":
                assert entry.get("active_revocation_verified"), entry
                result["active_revocation_verified"] = True
            agent = agents[0 if case == "allow" else 1]
            assert entry["pid"] == agent["pid"]
            result["cases"].append(entry)
            print(json.dumps(entry), flush=True)
            if capture:
                capture.selected.pop(agent["pid"])
                capture.set_intercept(",".join(map(str, capture.selected)) or "0")
        denied = [
            flow
            for flow in result.get("native_flows", [])
            if flow["agent_id"] == agents[1]["agent_id"]
        ]
        if args.datagram:
            assert result["cases"][1].get("policy_denied"), result["cases"]
        else:
            assert denied and all(
                "SOCKS request rejected" in flow.get("error", "") for flow in denied
            ), denied
            assert (
                "policy_denied: workspace policy denied this operation"
                in (root / "host-0.log").read_text()
            )
        result["policy_denial_verified"] = True
        result["policy"] = policy
        await asyncio.sleep(
            11
        )  # wait for real ledger persistence, not in-memory counters.
        traffic_class = "direct_network" if args.direct_datagram else "virtual_network"
        rows = await sql(
            f"SELECT COALESCE(json_agg(t),'[]'::json) FROM (SELECT source_id,destination_id,payload_bytes,payload_frames,billable_bytes FROM traffic_usage_hourly WHERE traffic_class='{traffic_class}') t;"
        )
        result["persisted_traffic"] = json.loads(rows)
        result["traffic_api"] = await api(proxy_a, base + "/traffic")
        result["agent_traffic_api"] = await api(proxy_a, base + "/traffic/agents")
        detail = result["agent_traffic_api"]["traffic"]
        assert sum(row["payload_bytes"] for row in detail) == 30, detail
        assert all(
            agents[0]["agent_id"] in (row["source_id"], row["destination_id"])
            for row in detail
        ), detail
        totals = {}
        for row in result["persisted_traffic"]:
            source = row["source_id"]
            totals[source] = totals.get(source, 0) + row["payload_bytes"]
        assert totals == {
            f"srv_lab_{token}_0": 12,
            (
                f"{target_ip}:{target['port']}"
                if args.direct_datagram
                else f"srv_lab_{token}_1"
            ): 18,
        }, totals
        if args.direct_datagram:
            assert all(
                row["billable_bytes"] == 0 for row in result["persisted_traffic"]
            )
            pending = list((root / "host-0/.treer/network-usage").glob("*.json"))
            assert not pending, pending
            receipts = json.loads(
                await sql(
                    "SELECT COALESCE(json_agg(t),'[]'::json) FROM (SELECT sent_bytes,received_bytes,closed_at IS NOT NULL AS closed FROM network_usage_receipts) t;"
                )
            )
            assert receipts == [
                {"sent_bytes": 12, "received_bytes": 18, "closed": True}
            ], receipts
            result["persisted_usage_receipts"] = receipts
            result["durable_usage_ack_verified"] = True
        result["passed"] = all(case["passed"] for case in result["cases"])
        assert result["passed"], result["cases"]
    finally:
        if capture:
            await capture.stop()
        for agent in agents:
            with contextlib.suppress(Exception):
                await api(
                    proxy_a,
                    f"/api/workspaces/{workspace}/agents/{agent['agent_id']}/stop",
                    {},
                    "POST",
                )
        if guest:
            with contextlib.suppress(asyncio.TimeoutError):
                await asyncio.wait_for(guest.communicate(), 5)
            if guest.returncode is None:
                guest.kill()
                await guest.wait()
        for proc in reversed(processes):
            if proc.returncode is None:
                proc.terminate()
                with contextlib.suppress(asyncio.TimeoutError):
                    await asyncio.wait_for(proc.wait(), 5)
                if proc.returncode is None:
                    proc.kill()
                    await proc.wait()
        for log in logs:
            log.close()
        if started_nats:
            await command("docker", "stop", "--time", "3", nats_container)
        if created_database:
            await sql(f'DROP DATABASE "{database}" WITH (FORCE);', db="postgres")
        result["cleanup"] = (
            "temporary Hosts/Proxies stopped; disposable DB and NATS stream deleted"
        )
        (root / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        Path(args.output).write_text(json.dumps(result, indent=2) + "\n")
        print(f"Evidence: {args.output}", flush=True)
    return 0


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--machine", default="treer")
    parser.add_argument(
        "--direct-datagram",
        action="store_true",
        help="Test Direct UDP durable usage acknowledgements; requires --datagram",
    )
    parser.add_argument(
        "--datagram",
        action="store_true",
        help="Test owned UDP transport without native capture",
    )
    parser.add_argument("--guest-ip", default="192.168.64.3")
    parser.add_argument(
        "--local-target",
        action="store_true",
        help="Use a loopback target; does not verify Apple guest connectivity",
    )
    parser.add_argument("--postgres-container", default="treer-postgres-test")
    parser.add_argument("--postgres-port", type=int, default=55432)
    parser.add_argument("--nats-image", default="nats:2.14.4-alpine3.22")
    parser.add_argument(
        "--half-close",
        action="store_true",
        help="Reproduce the upstream native helper half-close failure",
    )
    parser.add_argument("--output", default="output/network-research/macos-live.json")
    parser.add_argument(
        "--revoke-active",
        action="store_true",
        help="Revoke Policy while an allowed TCP stream remains open",
    )
    arguments = parser.parse_args()
    if arguments.direct_datagram and not arguments.datagram:
        parser.error("--direct-datagram requires --datagram")
    raise SystemExit(asyncio.run(main(arguments)))
