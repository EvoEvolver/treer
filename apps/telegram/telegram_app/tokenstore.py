"""The bot token kept by the settings App: one private file in the state directory.

Write-only by design: callers can store, replace or clear the token and learn only a fingerprint-free summary
(the last four characters); nothing here ever returns a token to an HTTP client. The file is created with mode 0600
through an exclusive temporary file in the same directory, flushed to disk and renamed over the old one, so a reader
(the bridge) sees either the old or the new token, never a partial one. Symlinks are refused in both directions.
"""
from __future__ import annotations

import os
import re
import stat
import uuid
from pathlib import Path

from .common import BridgeError

FILENAME = "bot-token"
TOKEN_SHAPE = re.compile(r"^[0-9]{3,20}:[A-Za-z0-9_-]{20,128}$")   # <bot id>:<secret>, the shape BotFather issues


def valid_shape(token: str) -> bool:
    return TOKEN_SHAPE.fullmatch(token) is not None


def last4(token: str) -> str:
    return token[-4:]


def _fsync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


class TokenStore:
    def __init__(self, state_dir: Path) -> None:
        self.directory = Path(state_dir)
        self.path = self.directory / FILENAME

    def _check(self, info: os.stat_result) -> None:
        if not stat.S_ISREG(info.st_mode):
            raise BridgeError("the stored bot token is not a regular file")
        if info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise BridgeError("the stored bot token must be owned by this user with mode 0600")

    def exists(self) -> bool:
        try:
            os.lstat(self.path)
            return True
        except FileNotFoundError:
            return False

    def read(self) -> str | None:
        """The stored token, or None when there is none. Unsafe files raise instead of being used."""
        try:
            info = os.lstat(self.path)
        except FileNotFoundError:
            return None
        self._check(info)
        if info.st_size == 0 or info.st_size > 512:
            raise BridgeError("the stored bot token has an invalid size")
        try:
            with open(self.path, "rb") as handle:
                return handle.read(513).decode("ascii").strip()
        except (OSError, UnicodeDecodeError) as error:
            raise BridgeError("the stored bot token cannot be read") from error

    def write(self, token: str) -> None:
        if not valid_shape(token):
            raise BridgeError("not a bot token")
        self.directory.mkdir(parents=True, exist_ok=True, mode=0o700)
        try:
            self._check(os.lstat(self.path))        # refuse to replace a symlink or foreign file
        except FileNotFoundError:
            pass
        temporary = self.directory / f".{FILENAME}.{uuid.uuid4().hex}"
        descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        try:
            with os.fdopen(descriptor, "wb") as handle:
                handle.write(token.encode("ascii") + b"\n")
                handle.flush()
                os.fsync(handle.fileno())
            os.replace(temporary, self.path)
            _fsync_directory(self.directory)
        finally:
            try:
                os.unlink(temporary)
            except FileNotFoundError:
                pass

    def clear(self) -> bool:
        """Overwrite, then delete. Returns whether a token was stored."""
        try:
            info = os.lstat(self.path)
        except FileNotFoundError:
            return False
        self._check(info)
        with open(self.path, "r+b") as handle:
            handle.write(b"0" * max(1, info.st_size))
            handle.flush()
            os.fsync(handle.fileno())
        os.unlink(self.path)
        _fsync_directory(self.directory)
        return True
