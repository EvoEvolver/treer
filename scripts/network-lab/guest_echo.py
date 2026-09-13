"""Temporary raw echo target; the lab stops it through stdin EOF."""

import asyncio
import json
import sys


async def main():
    if sys.argv[2] == "udp":

        class Echo(asyncio.DatagramProtocol):
            def connection_made(self, transport):
                self.transport = transport

            def datagram_received(self, data, address):
                self.transport.sendto(b"GUEST:" + data[::-1], address)

        transport, _ = await asyncio.get_running_loop().create_datagram_endpoint(
            Echo, local_addr=(sys.argv[1], 0)
        )
        print(json.dumps({"port": transport.get_extra_info("sockname")[1]}), flush=True)
        await asyncio.to_thread(sys.stdin.buffer.read)
        transport.close()
        return

    async def echo(reader, writer):
        try:
            data = (
                await reader.read()
                if sys.argv[2] == "yes"
                else await reader.readexactly(12)
            )
        except asyncio.IncompleteReadError:
            writer.close()
            return
        writer.write(b"GUEST:" + data[::-1])
        await writer.drain()
        if sys.argv[2] == "hold":
            await reader.read()
        writer.close()

    server = await asyncio.start_server(echo, sys.argv[1], 0)
    print(json.dumps({"port": server.sockets[0].getsockname()[1]}), flush=True)
    await asyncio.to_thread(sys.stdin.buffer.read)
    server.close()
    await server.wait_closed()


asyncio.run(main())
