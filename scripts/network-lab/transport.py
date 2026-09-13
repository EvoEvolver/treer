"""Raw TCP/SOCKS transport shared by the opt-in network probes."""

import asyncio
import contextlib
import struct

PROXY_ENV = (
    "ALL_PROXY",
    "all_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "NO_PROXY",
    "no_proxy",
)


async def copy(reader, writer):
    total = 0
    while data := await reader.read(65536):
        writer.write(data)
        await writer.drain()
        total += len(data)
    with contextlib.suppress(Exception):
        writer.write_eof()
    return total


async def splice(reader, writer, other_reader, other_writer):
    try:
        return await asyncio.gather(
            copy(reader, other_writer), copy(other_reader, writer)
        )
    finally:
        writer.close()
        other_writer.close()


async def socks_connect(host, port, agent, proxy_port):
    r, w = await asyncio.open_connection("127.0.0.1", proxy_port)
    try:
        w.write(b"\x05\x01\x02")
        await w.drain()
        assert await r.readexactly(2) == b"\x05\x02"
        user = agent.encode()
        w.write(b"\x01" + bytes([len(user)]) + user + b"\x05treer")
        await w.drain()
        assert await r.readexactly(2) == b"\x01\x00"
        name = host.encode("idna")
        w.write(
            b"\x05\x01\x00\x03" + bytes([len(name)]) + name + struct.pack("!H", port)
        )
        await w.drain()
        reply = await r.readexactly(10)
        if reply[1]:
            raise ConnectionRefusedError(f"SOCKS request rejected (reply={reply[1]})")
        return r, w
    except BaseException:
        w.close()
        raise
