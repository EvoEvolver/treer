"""Experimental, cooperative PID capture adapter; not a security boundary.

No descendant tracking, registration ACK, or fail-closed guarantee. One active
instance per machine. The caller registers Host-provided PIDs before release.
"""

import asyncio
import ipaddress

import mitmproxy_rs
from dnslib import QTYPE, RR, A, DNSRecord
from transport import socks_connect, splice


class MacCapture:
    def __init__(self, socks_port, evidence):
        self.socks_port = socks_port
        self.evidence = evidence
        self.selected = {}
        self.tasks = set()
        self.dns_names = {}
        self.dns_addresses = {}
        self.redirector = None

    async def start(self):
        self.redirector = await mitmproxy_rs.local.start_local_redirector(
            lambda stream: self.tracked(self.tcp, stream),
            lambda stream: self.tracked(self.udp, stream),
        )
        self.set_intercept("0")

    def set_intercept(self, spec):
        self.redirector.set_intercept(spec)

    async def stop(self):
        if self.redirector is None:
            return
        self.set_intercept("0")
        tasks = list(self.tasks)
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
        self.redirector.close()
        await self.redirector.wait_closed()

    async def tracked(self, handler, stream):
        # Native read futures do not retain all Python tasks. Keep strong refs.
        task = asyncio.current_task()
        self.tasks.add(task)
        try:
            await handler(stream)
        finally:
            stream.close()
            self.tasks.discard(task)

    async def tcp(self, stream):
        pid = stream.get_extra_info("pid", None)
        agent = self.selected.get(pid)
        if agent is None:
            return
        host, port = stream.get_extra_info("remote_endpoint")
        host = self.dns_names.get(host, host)
        record = {"pid": pid, "agent_id": agent, "host": host, "port": port}
        self.evidence.setdefault("native_flows", []).append(record)
        try:
            reader, writer = await socks_connect(host, port, agent, self.socks_port)
            sent, received = await splice(stream, stream, reader, writer)
            record.update(sent_bytes=sent, received_bytes=received)
        except (OSError, asyncio.IncompleteReadError) as error:
            record["error"] = str(error)

    async def udp(self, stream):
        pid = stream.get_extra_info("pid", None)
        destination = stream.get_extra_info("sockname")
        if destination[1] != 53 or pid not in self.selected:
            self.evidence.setdefault("udp_blocked", []).append(pid)
            return
        try:
            while packet := await asyncio.wait_for(stream.read(65535), 5):
                query = DNSRecord.parse(packet)
                name = str(query.q.qname).rstrip(".").lower()
                reply = query.reply()
                if not name.endswith(".treer.invalid"):
                    # Public DNS uses real answers because macOS shares its cache.
                    # Do not infer a public domain identity from a shared IP/cache.
                    wire = await asyncio.to_thread(
                        query.send, destination[0], destination[1], timeout=3
                    )
                    reply = DNSRecord.parse(wire)
                elif query.q.qtype == QTYPE.A:
                    if name not in self.dns_addresses:
                        address = str(
                            ipaddress.IPv4Address("198.19.0.1")
                            + len(self.dns_addresses)
                        )
                        self.dns_addresses[name] = address
                        self.dns_names[address] = name
                    reply.add_answer(
                        RR(
                            query.q.qname,
                            QTYPE.A,
                            ttl=0,
                            rdata=A(self.dns_addresses[name]),
                        )
                    )
                self.evidence.setdefault("native_dns", []).append(
                    {"pid": pid, "name": name, "qtype": query.q.qtype}
                )
                stream.write(reply.pack())
                await stream.drain()
        except (OSError, asyncio.TimeoutError):
            pass
