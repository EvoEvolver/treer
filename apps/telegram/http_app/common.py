"""Bounds shared by the public data plane and the private bridge."""
import collections
import hashlib
import json
import re
import threading
import time

MAX_REQUEST = 65536
MAX_BODY = 60000
MAX_ITEMS = 100
MAX_CLIENT_BYTES = 4 * 1024 * 1024
MAX_TOTAL_BYTES = 256 * 1024 * 1024
ID = re.compile(r"[a-f0-9]{32}\Z")
TOKEN = re.compile(r"[A-Za-z0-9_-]{43}\Z")


class Error(Exception):
    def __init__(self, status, code, message, retry=None):
        self.status, self.code, self.message, self.retry = status, code, message, retry
        super().__init__(code)


def invalid():
    return Error(401, "unauthorized", "Invalid credentials")


def digest(value):
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def text(value, maximum, *, empty=False):
    try:
        size = len(value.encode("utf-8")) if isinstance(value, str) else maximum + 1
    except UnicodeError:
        size = maximum + 1
    if (not isinstance(value, str) or (not empty and not value) or size > maximum
            or any(ord(ch) < 32 and ch not in "\n\t" for ch in value) or "\x7f" in value):
        raise Error(400, "invalid_request", "Invalid text field")
    return value


def fields(value, required, optional=()):
    if not isinstance(value, dict) or not set(required) <= value.keys() or value.keys() - set(required) - set(optional):
        raise Error(400, "invalid_request", "Invalid JSON fields")
    return value


def strict_json(raw):
    def pairs(items):
        result = {}
        for key, value in items:
            if key in result:
                raise ValueError("duplicate key")
            result[key] = value
        return result
    try:
        return json.loads(raw.decode("utf-8"), object_pairs_hook=pairs,
                          parse_constant=lambda _: (_ for _ in ()).throw(ValueError("nonfinite")))
    except (ValueError, UnicodeError):
        raise Error(400, "invalid_request", "Invalid JSON") from None


class Limiter:
    """Bounded process-local windows; failed anonymous traffic never writes SQLite."""
    def __init__(self, clock=time.monotonic, capacity=1024):
        self.clock, self.capacity = clock, capacity
        self.rows = collections.OrderedDict()
        self.lock = threading.Lock()

    def take(self, key, limit, seconds):
        now = self.clock()
        with self.lock:
            row = self.rows.pop(key, collections.deque())
            while row and row[0] <= now - seconds:
                row.popleft()
            allowed = len(row) < limit
            if allowed:
                row.append(now)
            self.rows[key] = row
            while len(self.rows) > self.capacity:
                self.rows.popitem(last=False)
            return allowed
