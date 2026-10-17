"""Public data listener. Deliberately no HTML, health, Core, or management routes."""
import argparse
import collections
import contextlib
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
import sqlite3
from pathlib import Path
import threading
import time
import urllib.parse

from .common import Error, Limiter, MAX_REQUEST, fields, invalid, strict_json, write_backoff
from . import ipc
from .store import QueueStore, PublicQueues

ROUTES = {("POST", "/v1/pair"), ("POST", "/v1/inbox"), ("GET", "/v1/outbox"), ("POST", "/v1/outbox/ack")}


def wire(value):
    """Keep epoch arithmetic private and expose UTC RFC 3339 on the wire."""
    if isinstance(value, list):
        return [wire(item) for item in value]
    if isinstance(value, dict):
        return {key: datetime.fromtimestamp(item, timezone.utc).isoformat().replace("+00:00", "Z")
                if key in ("at", "created_at", "expires_at", "failed_at", "last_at") and type(item) in (int, float)
                else wire(item) for key, item in value.items()}
    return value


class Application:
    def __init__(self, queues, pair_socket):
        self.queues, self.pair_socket = PublicQueues(queues), pair_socket
        self.limits = Limiter()
        self.poll_counts = collections.Counter()
        self.poll_lock = threading.Lock()
        self.pair_lock = threading.Lock()
        self.pair_backoff_until = 0
        self.diagnostic_root = queues.path.parent
        self.poll_slots = threading.BoundedSemaphore(16)

    def authenticate(self, headers):
        values = headers.get_all("Authorization") or []
        token = values[0][7:] if len(values) == 1 and values[0].startswith("Bearer ") else None
        try:
            return self.queues.authenticate(token), token
        except Error:
            if not self.limits.take("anonymous", 60, 60):
                raise Error(429, "rate_limited", "Too many requests", 60) from None
            raise

    def admit_message(self, client):
        if not self.limits.take(("inbox", client), 60, 3600) or not self.limits.take("inbox-global", 1000, 60):
            raise Error(429, "rate_limited", "Too many messages", 60)

    @contextlib.contextmanager
    def polling(self, client, wait):
        admitted = False
        if wait:
            with self.poll_lock:
                if self.poll_counts[client] >= 2 or not self.poll_slots.acquire(blocking=False):
                    raise Error(429, "poll_capacity", "Use timeout=0 or retry later", 1)
                self.poll_counts[client] += 1
                admitted = True
        try:
            yield
        finally:
            if admitted:
                with self.poll_lock:
                    self.poll_counts[client] -= 1
                    if self.poll_counts[client] == 0:
                        del self.poll_counts[client]
                    self.poll_slots.release()

    def poll(self, client, token, after, wait):
        deadline = time.monotonic() + wait
        with self.polling(client, wait):
            while True:
                # Every iteration closes its SQLite connections, including empty polls.
                self.queues.authenticate(token)
                result = self.queues.poll(client, after)
                if result["items"] or result["failures"] or time.monotonic() >= deadline:
                    self.queues.authenticate(token)  # rotation/revocation while waiting
                    self.queues.available(client)
                    for item in result["items"]:
                        item["created_at"] = datetime.fromtimestamp(item["created_at"], timezone.utc).isoformat().replace("+00:00", "Z")
                    return result
                time.sleep(min(0.2, max(0, deadline - time.monotonic())))


class Server(ThreadingHTTPServer):
    daemon_threads = True
    block_on_close = False

    def __init__(self, address, handler, app):
        self.app = app
        self.connections = threading.BoundedSemaphore(40)
        super().__init__(address, handler)

    def get_request(self):
        connection, address = super().get_request()
        connection.settimeout(5)
        return connection, address

    def process_request(self, request, address):
        if not self.connections.acquire(blocking=False):
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, address)
        except Exception:
            self.connections.release()
            raise

    def process_request_thread(self, request, address):
        try:
            super().process_request_thread(request, address)
        finally:
            self.connections.release()

    def handle_error(self, *_):
        pass


class Handler(BaseHTTPRequestHandler):
    server_version = "TreerHttpEndpoint"
    sys_version = ""

    def log_message(self, *_):
        pass

    def reply(self, status, value, retry=None):
        raw = bytearray(json.dumps(wire(value), ensure_ascii=False).encode())
        self.send_response(status)
        for key, val in (("Content-Type", "application/json; charset=utf-8"), ("Cache-Control", "no-store"),
                         ("X-Content-Type-Options", "nosniff"), ("Content-Length", str(len(raw))), ("Connection", "close")):
            self.send_header(key, val)
        if retry is not None:
            self.send_header("Retry-After", str(retry))
        self.end_headers()
        self.close_connection = True
        try:
            if self.command != "HEAD":
                self.wfile.write(raw)
        finally:
            raw[:] = b"\0" * len(raw)
            value.pop("token", None)

    def send_error(self, code, message=None, explain=None):
        status = 404 if code in (404, 501) else code
        self.reply(status, {"error": {"code": "invalid_request", "message": "Request rejected"}})

    def body(self, limit=MAX_REQUEST):
        lengths = self.headers.get_all("Content-Length") or []
        if len(lengths) != 1 or not lengths[0].isascii() or not lengths[0].isdigit() or self.headers.get("Transfer-Encoding"):
            raise Error(400, "invalid_request", "One Content-Length required")
        length = int(lengths[0])
        if not 0 < length <= limit:
            raise Error(413, "body_too_large", "Request body too large")
        if self.headers.get("Content-Encoding"):
            raise Error(415, "invalid_encoding", "Compressed requests are not supported")
        if self.headers.get("Content-Type", "").split(";")[0].strip() != "application/json":
            raise Error(415, "invalid_content_type", "application/json required")
        try:
            raw = bytearray()
            deadline = time.monotonic() + 5
            while len(raw) < length:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError()
                self.connection.settimeout(remaining)
                chunk = self.rfile.read1(min(16384, length - len(raw)))
                if not chunk:
                    break
                raw.extend(chunk)
        except OSError:
            raise Error(408, "request_timeout", "Request body timed out") from None
        if len(raw) != length:
            raise Error(400, "invalid_request", "Incomplete body")
        return strict_json(raw)

    def dispatch(self):
        try:
            app = self.server.app
            url = urllib.parse.urlsplit(self.path)
            if url.scheme or url.netloc or (self.command, url.path) not in ROUTES:
                raise Error(404, "not_found", "Route not found")
            if url.path == "/v1/pair":
                if url.query:
                    raise Error(400, "invalid_request", "Unexpected query")
                if not app.limits.take("pair", 10, 60):
                    with app.pair_lock:
                        if time.time() >= app.pair_backoff_until:
                            app.pair_backoff_until = time.time() + 60
                            write_backoff(app.diagnostic_root, 'pair-public-backoff.json', app.pair_backoff_until)
                    raise Error(429, "pair_backoff", "Pairing temporarily cooling down", 60)
                body = fields(self.body(2048), ("code",))
                if not isinstance(body["code"], str) or len(body["code"]) != 43:
                    raise invalid()
                try:
                    self.reply(200, ipc.redeem(app.pair_socket, body["code"]))
                finally:
                    body.clear()
                return
            client, token = app.authenticate(self.headers)
            client_id = client["id"]
            if not app.limits.take(("request", client_id), 240, 60):
                raise Error(429, "rate_limited", "Too many requests", 60)
            if url.path == "/v1/outbox":
                try:
                    values = urllib.parse.parse_qs(url.query, strict_parsing=True, keep_blank_values=True)
                    if values.keys() - {"after", "timeout"} or any(len(v) != 1 for v in values.values()):
                        raise ValueError()
                    after = int(values.get("after", ["0"])[0])
                    wait = int(values.get("timeout", ["0"])[0])
                    if not 0 <= after < 2 ** 63 or not 0 <= wait <= 30:
                        raise ValueError()
                except ValueError:
                    raise Error(400, "invalid_query", "Invalid cursor or timeout") from None
                self.reply(200, app.poll(client_id, token, after, wait))
                return
            if url.query:
                raise Error(400, "invalid_request", "Unexpected query")
            body = self.body()
            if url.path == "/v1/inbox":
                message = app.queues.enqueue(client_id, body, admit=lambda: app.admit_message(client_id),
                                             expected_generation=client["generation"])
                self.reply(202, {"message_id": message})
            else:
                fields(body, ("up_to",))
                app.queues.ack(client_id, body["up_to"], expected_generation=client["generation"])
                self.reply(200, {"ok": True})
        except Error as error:
            self.reply(error.status, {"error": {"code": error.code, "message": error.message}}, error.retry)
        except (OSError, ValueError, sqlite3.Error):
            self.reply(503, {"error": {"code": "unavailable", "message": "Endpoint unavailable"}})

    do_GET = do_POST = do_HEAD = do_OPTIONS = do_PUT = do_PATCH = do_DELETE = do_TRACE = do_CONNECT = dispatch


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state-dir", default=os.environ.get("TREER_APP_STATE_DIR"), required=not os.environ.get("TREER_APP_STATE_DIR"))
    parser.add_argument("--listen", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=9442)
    args = parser.parse_args()
    root = Path(args.state_dir)
    app = Application(QueueStore(root / "data.sqlite"), root / "pair.sock")
    with Server((args.listen, args.port), Handler, app) as server:
        server.serve_forever()
