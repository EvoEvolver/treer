"""Narrow local pairing RPC. No admin API, identity credentials or salt exports."""
import os
import ctypes
import struct
from pathlib import Path
import socket
import socketserver
import stat
import threading

from .common import Error, fields, strict_json
import json


def peer_uid(connection):
    if hasattr(socket, "SO_PEERCRED"):
        raw = connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i"))
        _pid, uid, _gid = struct.unpack("3i", raw)
        return uid
    # macOS/BSD: libc's getpeereid avoids depending on xucred struct layout.
    libc = ctypes.CDLL(None, use_errno=True)
    function = getattr(libc, "getpeereid", None)
    if function is None:
        raise OSError("Peer credentials unavailable")
    function.argtypes = [ctypes.c_int, ctypes.POINTER(ctypes.c_uint), ctypes.POINTER(ctypes.c_uint)]
    function.restype = ctypes.c_int
    uid, gid = ctypes.c_uint(), ctypes.c_uint()
    if function(connection.fileno(), ctypes.byref(uid), ctypes.byref(gid)) != 0:
        raise OSError("Peer credentials unavailable")
    return uid.value


def line(stream, limit=4096):
    data = stream.readline(limit + 1)
    if len(data) > limit or not data.endswith(b"\n"):
        raise Error(400, "invalid_request", "Invalid pairing envelope")
    return strict_json(data)


class PairHandler(socketserver.StreamRequestHandler):
    def handle(self):
        self.connection.settimeout(5)
        try:
            value = fields(line(self.rfile), ("code",))
            result = {"ok": self.server.admin.redeem(value["code"])}
        except Error as error:
            result = {"error": {"status": error.status, "code": error.code, "message": error.message, "retry": error.retry}}
        except (OSError, ValueError):
            result = {"error": {"status": 503, "code": "pair_unavailable", "message": "Pairing unavailable"}}
        try:
            buffer = bytearray(json.dumps(result).encode() + b"\n")
            try:
                self.wfile.write(buffer)
            finally:
                buffer[:] = b"\0" * len(buffer)
                result.clear()
        except OSError:
            pass


class PairServer(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True
    block_on_close = False

    def __init__(self, path, admin):
        self.admin = admin
        self.slots = threading.BoundedSemaphore(4)
        path = Path(path)
        if path.exists() or path.is_symlink():
            info = path.lstat()
            if not stat.S_ISSOCK(info.st_mode) or info.st_uid != os.getuid():
                raise ValueError("pair socket must be private")
            with socket.socket(socket.AF_UNIX) as probe:
                try:
                    probe.connect(str(path))
                except ConnectionRefusedError:
                    path.unlink()
                else:
                    raise ValueError("pair validator is already running")
        super().__init__(str(path), PairHandler)
        path.chmod(0o600)

    def verify_request(self, request, address):
        try:
            return peer_uid(request) == os.getuid()
        except OSError:
            return False

    def process_request(self, request, address):
        if not self.slots.acquire(blocking=False):
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, address)
        except Exception:
            self.slots.release()
            raise

    def process_request_thread(self, request, address):
        try:
            super().process_request_thread(request, address)
        finally:
            self.slots.release()

    def handle_error(self, *_):
        pass


def redeem(path, code):
    path = Path(path)
    try:
        info = path.lstat()
        if not stat.S_ISSOCK(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise OSError("not a private socket")
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(5)
            connection.connect(str(path))
            connection.sendall(json.dumps({"code": code}).encode() + b"\n")
            with connection.makefile("rb") as stream:
                result = line(stream)
        if "error" in result:
            error = result["error"]
            raise Error(error["status"], error["code"], error["message"], error.get("retry"))
        return result["ok"]
    except (OSError, KeyError, ValueError):
        raise Error(503, "pair_unavailable", "Pairing unavailable") from None
