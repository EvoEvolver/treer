# Cursor AIS

Compatibility sidecar. Prefer **New → Cursor thread** in the Treer control
plane (`kind=acp`), which starts `treer-acp --harness cursor` and the Host
thread UI.

Thin Treer Agent Interface adapter over Cursor CLI's Agent Client Protocol
server (`cursor-agent acp`). One Treer Agent owns one ACP session.

Cursor does not ship a Codex-style app-server. Official ACP over stdio is the
integration surface. The binary is `cursor-agent`, not `agent`: Grok Build also
installs an `agent` symlink.

## Launch

```bash
treer agent admin profile create "Cursor AIS" \
  --description "Cursor ACP session with a per-Agent AIS adapter" \
  --cwd treer apps/cursor-ais/scripts/treer-agent.sh
treer agent admin profile launch "Cursor AIS" --name cursor-ais
```

Authenticate the CLI once with `cursor-agent login`, or set `CURSOR_API_KEY` /
`CURSOR_AUTH_TOKEN`. Cursor IDE login is a separate credential and does not
satisfy the CLI. Optional model override: `CURSOR_AIS_MODEL`.

The sidecar listens on `127.0.0.1` with `AIS_PORT` (default `0`) and registers
`prompt.submit`, `transcript.read`, `state.observe`, and `abort`.
