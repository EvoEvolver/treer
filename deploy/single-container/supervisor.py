#!/usr/bin/env python3
"""Fixed two-process container entrypoint. Never invoke a shell or log secrets."""
import fcntl
import json
import os
from pathlib import Path
import pwd
import secrets
import signal
import stat
import subprocess
import sys
import time
from urllib.parse import quote


class StartupError(Exception):
    pass


def log(message):
    print("treer-single: " + message, flush=True)


def private_directory(path, uid=0, gid=0):
    # All parents are fixed, already checked directories, not user-selected paths.
    try:
        path.mkdir(mode=0o700)
    except FileExistsError:
        pass
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid not in (0, uid):
        raise StartupError("unsafe data directory ownership or type")
    os.chown(path, 0, 0, follow_symlinks=False)
    path.chmod(0o700)
    os.chown(path, uid, gid, follow_symlinks=False)


def read_secret(path, owner=None):
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except OSError:
        raise StartupError("secret file cannot be opened safely") from None
    info = os.fstat(fd)
    if (not stat.S_ISREG(info.st_mode) or info.st_mode & 0o077
            or info.st_uid != (os.geteuid() if owner is None else owner) or info.st_nlink != 1):
        os.close(fd)
        raise StartupError("secret file must be owned by entrypoint user and private (0600)")
    with os.fdopen(fd, "rb") as stream:
        data = stream.read(16385)
    if len(data) > 16384:
        raise StartupError("secret file too large")
    try:
        value = data.decode("utf-8").removesuffix("\n")
    except UnicodeError:
        raise StartupError("secret file must contain UTF-8") from None
    if not value or "\x00" in value or "\n" in value or "\r" in value:
        raise StartupError("secret must be a nonempty single line")
    return value


def write_secret(path, value, owner=None):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w") as stream:
        if owner is not None:
            os.fchown(stream.fileno(), owner, -1)
        stream.write(value + "\n")
        stream.flush()
        os.fsync(stream.fileno())
    directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def configured_secret(env, name):
    if name in env:
        value = env[name]
        if not value or any(c in value for c in "\x00\r\n"):
            raise StartupError(name + " must be a nonempty single line")
        return value
    if name + "_FILE" in env:
        return read_secret(Path(env[name + "_FILE"]))
    return None


def persistent_secret(path, configured=None, *, first_start=False, owner=None):
    if path.exists() or path.is_symlink():
        existing = read_secret(path, owner)
        if configured is not None and not secrets.compare_digest(existing, configured):
            raise StartupError("database password differs from persisted configuration")
        return existing
    if not first_start:
        raise StartupError("persisted database password missing; restore backup, do not reinitialize")
    value = configured or secrets.token_urlsafe(48)
    write_secret(path, value, owner)
    return value


def sql_literal(value):
    return "'" + value.replace("'", "''") + "'"


def check_volume_links(data):
    for directory, dirs, files in os.walk(data, followlinks=False):
        for name in dirs + files:
            entry = Path(directory) / name
            if entry.is_symlink():
                try:
                    inside = entry.resolve().is_relative_to(data.resolve())
                except (OSError, RuntimeError):
                    inside = False
                if not inside:
                    raise StartupError("data volume contains an external symbolic link")


def check_image_version(private, manifest):
    # Release versions are monotonic. Same-version Git ancestry is NOT inferable.
    def version(value):
        parts = value.split(".")
        if len(parts) != 3 or any(not p.isdigit() for p in parts):
            raise StartupError("invalid image release version")
        return tuple(map(int, parts))
    current = json.loads(manifest.read_text())["version"]
    marker = private / "image-version"
    if marker.exists() or marker.is_symlink():
        previous = read_secret(marker)
        if version(previous) > version(current):
            raise StartupError("image downgrade refused; restore the matching backup into a separate volume")
        if previous == current:
            return
    version(current)
    temp = private / ("image-version-" + secrets.token_hex(8))
    write_secret(temp, current)
    temp.replace(marker)


class Supervisor:
    def __init__(self, data=Path("/data"), env=None):
        self.data = data
        self.env = dict(os.environ if env is None else env)
        self.stopping = False
        self.pg = self.proxy = None
        self.pg_user = pwd.getpwnam("postgres")
        self.proxy_user = pwd.getpwnam("treer")
        self.pg_bin = Path("/usr/lib/postgresql/17/bin")
        self.pgdata = data / "pg"
        self.socket = data / "socket"
        self.private = data / "private"
        self.pg_env = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "HOME": str(self.pgdata)}
        self.lock_fd = None

    def signal_stop(self, *_):
        self.stopping = True

    def child_options(self, user, env):
        return dict(user=user.pw_uid, group=user.pw_gid, extra_groups=[],
                    env=env, cwd="/", start_new_session=True)

    def pg_command(self, name, args, *, input=None):
        try:
            result = subprocess.run([str(self.pg_bin / name), *args], input=input, text=True,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=120,
                                    **self.child_options(self.pg_user, self.pg_env))
        except (OSError, subprocess.TimeoutExpired):
            raise StartupError("PostgreSQL initialization command failed: " + name) from None
        if result.returncode:
            # SQL errors can quote passwords; do not forward stdout/stderr.
            raise StartupError("PostgreSQL initialization command failed: " + name)
        if self.stopping:
            raise StartupError("startup interrupted")
        return result.stdout.strip()

    def prepare(self):
        if os.geteuid() != 0:
            raise StartupError("entrypoint requires root to initialize the volume and launch separate users")
        if self.env.get("DATABASE_URL") or self.env.get("TREER_DISABLE_AUTH", "false").lower() != "false":
            raise StartupError("external DATABASE_URL and disabled authentication are unsupported in this image")
        if self.env.get("TREER_SINGLE_MAINTENANCE", "") not in ("", "restore"):
            raise StartupError("TREER_SINGLE_MAINTENANCE supports only restore")
        self.data.mkdir(mode=0o700, exist_ok=True)
        info = self.data.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0:
            raise StartupError("data volume root must be a real root-owned directory")
        self.lock_fd = os.open(self.data / ".lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
        info = os.fstat(self.lock_fd)
        if not stat.S_ISREG(info.st_mode) or info.st_uid != 0 or info.st_nlink != 1 or info.st_mode & 0o077:
            raise StartupError("unsafe data volume lock")
        try:
            fcntl.flock(self.lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise StartupError("data volume is already in use") from None
        # Do not change permissions or version markers before owning the lock.
        private_directory(self.data)
        check_volume_links(self.data)
        private_directory(self.private)
        self.private.chmod(0o711)
        # Traversal is required for the two fixed child accounts; no directory listing.
        self.data.chmod(0o711)
        private_directory(self.pgdata, self.pg_user.pw_uid, self.pg_user.pw_gid)
        private_directory(self.socket, self.pg_user.pw_uid, self.pg_user.pw_gid)
        marker = self.private / "initializing"
        complete = self.private / "initialized"
        if marker.exists() or marker.is_symlink():
            raise StartupError("interrupted database initialization; restore a backup or use a NEW empty volume")
        first = not any(self.pgdata.iterdir())
        if first and complete.exists():
            raise StartupError("initialized database is missing; restore backup")
        if not first:
            version = self.pgdata / "PG_VERSION"
            if version.is_symlink() or not version.is_file() or version.read_text().strip() != "17":
                raise StartupError("PostgreSQL major mismatch or incomplete data; dump/restore into a new volume")
            if not complete.is_file() or complete.is_symlink():
                raise StartupError("database was not initialized by this image; use documented restore procedure")
            if read_secret(complete) != "storage-v1-pg17":
                raise StartupError("unsupported data storage format; use a compatible image and backup")
        db_password = persistent_secret(self.private / "database-password",
                                        configured_secret(self.env, "POSTGRES_PASSWORD"), first_start=first,
                                        owner=self.proxy_user.pw_uid)
        admin_password = configured_secret(self.env, "ADMIN_PASSWORD")
        if admin_password is None:
            path = self.private / "admin-password"
            if not path.exists() and not path.is_symlink():
                if not first:
                    raise StartupError("ADMIN_PASSWORD missing; provide it or restore private/admin-password")
                write_secret(path, secrets.token_urlsafe(48), owner=self.proxy_user.pw_uid)
            admin_password = read_secret(path, owner=self.proxy_user.pw_uid)
        allowed = {"PATH", "LANG", "RUST_LOG", "SSL_CERT_FILE", "SSL_CERT_DIR",
                   "GITHUB_OAUTH_CLIENT_ID", "GITHUB_OAUTH_CLIENT_SECRET",
                   "GOOGLE_OAUTH_CLIENT_ID", "GOOGLE_OAUTH_CLIENT_SECRET",
                   "CLOUDFLARE_API_TOKEN", "CLOUDFLARE_ACCOUNT_ID"}
        self.proxy_env = {k: v for k, v in self.env.items()
                          if k in allowed or k.startswith("TREER_")}
        # This distribution never enables an updater or unauthenticated operation.
        for key in ("TREER_UPDATER_URL", "TREER_UPDATER_TOKEN", "TREER_ALLOW_UNAUTHENTICATED_LISTEN",
                    "TREER_SECURITY_HELP_FIXTURE", "TREER_DISABLE_AUTH"):
            self.proxy_env.pop(key, None)
        self.proxy_env.update(ADMIN_PASSWORD=admin_password,
                              DATABASE_URL="postgres://treer:" + quote(db_password, safe="") + "@127.0.0.1:5432/treer",
                              TREER_APP_DIST_DIR="/app/web", TREER_ARTIFACTS_DIR="/app/dist",
                              TREER_PROXY_LISTEN="0.0.0.0:8080", PORT="8080")
        # Defaults may be changed only via explicit public URLs, not request Host headers.
        self.proxy_env.setdefault("TREER_PROXY_PUBLIC_URL", "http://proxy.treer.localhost:8080/")
        self.proxy_env.setdefault("TREER_APP_PUBLIC_URL", "http://console.treer.localhost:8080/")
        self.proxy_env.setdefault("TREER_ENABLE_CORE_MESSAGES", "true")
        # Missing platforms fail closed, never bootstrap a different 'latest' commit.
        self.proxy_env.setdefault("TREER_RELEASE_ARTIFACT_BASE_URL", "http://127.0.0.1:9/unavailable/")
        self.proxy_env["HOME"] = "/home/treer"
        check_image_version(self.private, Path("/app/build.json"))
        if first:
            write_secret(marker, "initializing")
            self.pg_command("initdb", ["-D", str(self.pgdata), "--username=postgres",
                                       "--auth-local=peer", "--auth-host=scram-sha-256", "--no-instructions"])
            # PG's configuration is generated once, not replaced on every boot.
            with (self.pgdata / "postgresql.conf").open("a") as stream:
                stream.write("\nlisten_addresses = '127.0.0.1'\nport = 5432\n"
                             "unix_socket_directories = '" + str(self.socket) + "'\n"
                             "password_encryption = 'scram-sha-256'\n")
            self.start_pg()
            self.pg_command("psql", ["-h", str(self.socket), "-U", "postgres", "-d", "postgres",
                                     "-v", "ON_ERROR_STOP=1", "-f", "-"], input=(
                "CREATE ROLE treer LOGIN PASSWORD " + sql_literal(db_password)
                + " NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION;\n"
                "CREATE DATABASE treer OWNER treer;\nREVOKE ALL ON DATABASE treer FROM PUBLIC;\n"))
            write_secret(complete, "storage-v1-pg17")
            marker.unlink()
        else:
            self.start_pg()

    def start_pg(self):
        log("starting PostgreSQL")
        self.pg = subprocess.Popen([str(self.pg_bin / "postgres"), "-D", str(self.pgdata)],
                                   **self.child_options(self.pg_user, self.pg_env))
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline and not self.stopping:
            if self.pg.poll() is not None:
                raise StartupError("PostgreSQL exited before readiness")
            result = subprocess.run([str(self.pg_bin / "pg_isready"), "-q", "-h", str(self.socket)],
                                    timeout=3, **self.child_options(self.pg_user, self.pg_env))
            if result.returncode == 0:
                return
            time.sleep(0.2)
        raise StartupError("PostgreSQL readiness timed out or startup interrupted")

    def start_proxy(self):
        if self.stopping:
            return
        log("starting Proxy")
        self.proxy = subprocess.Popen(["/usr/local/bin/treer-proxy"],
                                      **self.child_options(self.proxy_user, self.proxy_env))

    @staticmethod
    def stop_child(child, name, sig, timeout, *, group=True):
        if child is None or child.poll() is not None:
            return
        log("stopping " + name)
        try:
            (os.killpg if group else os.kill)(child.pid, sig)
        except ProcessLookupError:
            pass
        try:
            child.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            log(name + " exceeded shutdown grace period")
            os.killpg(child.pid, signal.SIGKILL)
            child.wait(timeout=5)

    def shutdown(self):
        self.stop_child(self.proxy, "Proxy", signal.SIGTERM, 20)
        # SIGINT is PostgreSQL fast shutdown: abort connections and checkpoint.
        self.stop_child(self.pg, "PostgreSQL", signal.SIGINT, 40, group=False)

    def supervise(self):
        if self.env.get("TREER_SINGLE_MAINTENANCE") == "restore":
            log("restore maintenance: Proxy disabled; no HTTP service")
            while not self.stopping:
                if self.pg.poll() is not None:
                    raise StartupError("PostgreSQL exited during restore maintenance")
                time.sleep(0.2)
            return
        self.start_proxy()
        failures = []
        while not self.stopping:
            time.sleep(0.2)
            pg_failed = self.pg.poll() is not None
            if not pg_failed and self.proxy.poll() is None:
                continue
            now = time.monotonic()
            failures = [at for at in failures if now - at < 300]
            failures.append(now)
            if pg_failed:
                self.stop_child(self.proxy, "Proxy", signal.SIGTERM, 20)
            if len(failures) > 5:
                raise StartupError("repeated child crashes; exiting for operator recovery")
            log("child exited; backing off before restart")
            deadline = now + min(2 ** (len(failures) - 1), 16)
            while not self.stopping and time.monotonic() < deadline:
                time.sleep(0.2)
            if self.stopping:
                break
            if pg_failed:
                self.start_pg()
            self.start_proxy()


def main():
    os.umask(0o077)
    supervisor = Supervisor()
    signal.signal(signal.SIGTERM, supervisor.signal_stop)
    signal.signal(signal.SIGINT, supervisor.signal_stop)
    try:
        supervisor.prepare()
        supervisor.supervise()
        return 0
    except (StartupError, OSError, subprocess.SubprocessError) as error:
        # Never format arbitrary exception contents: they can contain env/SQL/argv.
        log(str(error) if isinstance(error, StartupError) else "system operation failed; inspect volume permissions")
        return 1
    finally:
        supervisor.shutdown()


if __name__ == "__main__":
    sys.exit(main())
