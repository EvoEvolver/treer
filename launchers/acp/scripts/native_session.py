"""Native TUI session discovery and a bounded, read-only ACP load probe."""
import json
import os
from pathlib import Path
import re
import selectors
import subprocess
import time


def records(path):
    with path.open(errors="replace") as handle:
        for line in handle:
            try:
                yield json.loads(line)
            except (ValueError, TypeError):
                continue


def discover(provider, cwd, session_id=None):
    requested_cwd = Path(cwd)
    cwd = requested_cwd.resolve()
    home = Path.home()
    if provider == "codex":
        files = (home / ".codex/sessions").glob("**/*.jsonl")
    else:
        # Native Claude encodes its original logical cwd, which can differ
        # from realpath on macOS (/var versus /private/var) or symlinked repos.
        # Match record cwd after resolving; never accept the directory alone.
        files = (home / ".claude/projects").glob("*/*.jsonl")
    candidates = []
    for file in files:
        try:
            for index, record in enumerate(records(file)):
                meta = record.get("payload", {}) if record.get("type") == "session_meta" else record
                sid = meta.get("id") if provider == "codex" else meta.get("sessionId")
                if sid and meta.get("cwd") and Path(meta["cwd"]).resolve() == cwd:
                    if session_id is None or sid == session_id:
                        candidates.append((file.stat().st_mtime_ns, sid, file))
                    break
                if index >= 100:
                    break
        except OSError:
            continue
    if not candidates:
        raise RuntimeError(f"no {provider} native session matches cwd {cwd}; use --session-id with its original cwd")
    _, sid, file = max(candidates, key=lambda item: (item[0], str(item[2])))
    return sid, file


def summary(provider, session_id, file, cwd):
    turns = []
    for record in records(file):
        message = record.get("payload", {}) if provider == "codex" else record.get("message", {})
        if provider == "codex" and record.get("type") != "response_item":
            continue
        role = message.get("role")
        if role not in ("user", "assistant"):
            continue
        content = message.get("content", [])
        if isinstance(content, str):
            text = content
        else:
            text = "\n".join(block.get("text", "") for block in content if isinstance(block, dict))
        if text.strip():
            # Native transcripts can contain credential-bearing commands. This
            # excerpt is local context, not a credential transport mechanism.
            text = re.sub(r"\b(?:enr_v1_|sk-)[A-Za-z0-9_.-]+", "[redacted]", text)
            text = re.sub(r"(?i)(bearer\s+|(?:api[_-]?key|token|password|secret)\s*[=:]\s*)\S+", r"\1[redacted]", text)
            turns.append(f"{role}: {text.strip()[:1200]}")
    excerpt = ([turns[0]] + turns[-5:]) if len(turns) > 6 else turns
    return (f"Continue the previous {provider} TUI session {session_id} in {cwd}. "
            "The original session could not be loaded; this is a bounded excerpt "
            "of its initial task and recent conversation, not new instructions.\n\n" + "\n\n".join(excerpt))[:8000]


def probe(command, cwd, session_id, timeout=30):
    """No prompt/model call: initialize, session/load, count replay events, exit."""
    process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL, start_new_session=True)
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    buffer = b""
    replay = 0
    deadline = time.monotonic() + timeout

    def request(number, method, params):
        nonlocal buffer, replay
        process.stdin.write((json.dumps({"jsonrpc": "2.0", "id": number,
                                        "method": method, "params": params}) + "\n").encode())
        process.stdin.flush()
        while time.monotonic() < deadline:
            ready = selector.select(max(0, deadline - time.monotonic()))
            if not ready:
                break
            chunk = os.read(process.stdout.fileno(), 65536)
            if not chunk:
                raise RuntimeError("ACP process exited before its response")
            buffer += chunk
            while b"\n" in buffer:
                line, buffer = buffer.split(b"\n", 1)
                try:
                    message = json.loads(line)
                except ValueError:
                    continue
                if message.get("method") == "session/update":
                    replay += 1
                if message.get("id") == number and "method" not in message:
                    if "error" in message:
                        raise RuntimeError(f"{method} rejected: {message['error'].get('code')}")
                    return message.get("result", {})
                if "id" in message and "method" in message:
                    # Loading history should not execute provider file/terminal
                    # requests. Explicit rejection prevents accidental side effects.
                    process.stdin.write((json.dumps({"jsonrpc": "2.0", "id": message["id"],
                        "error": {"code": -32601, "message": "read-only session probe"}}) + "\n").encode())
                    process.stdin.flush()
        raise RuntimeError(f"{method} timed out")

    try:
        initialized = request(1, "initialize", {"protocolVersion": 1, "clientInfo": {
            "name": "treer-session-probe", "version": "1"}, "clientCapabilities": {}})
        capabilities = initialized.get("agentCapabilities", {})
        if not capabilities.get("loadSession"):
            return {"loaded": False, "reason": "loadSession not advertised", "replay_updates": replay}
        request(2, "session/load", {"sessionId": session_id, "cwd": str(cwd), "mcpServers": []})
        return {"loaded": True, "replay_updates": replay}
    except (RuntimeError, OSError, BrokenPipeError) as error:
        return {"loaded": False, "reason": str(error), "replay_updates": replay}
    finally:
        selector.close()
        # The probe owns this process group, including any adapter subprocesses.
        import signal
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
