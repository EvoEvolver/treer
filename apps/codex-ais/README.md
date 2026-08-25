# Codex AIS

Thin Treer Agent Interface adapter over `codex app-server`. One Treer Agent
owns one Codex thread. Built-in `--kind codex` remains the TUI path.

## Launch

From a Host-relative checkout of this repository:

```bash
treer agent admin profile create "Codex AIS" \
  --description "Codex app-server with a per-Agent AIS adapter" \
  --cwd treer apps/codex-ais/scripts/treer-agent.sh
treer agent admin profile launch "Codex AIS" --name codex-ais
```

Optional model override: `CODEX_AIS_MODEL` or `AIS_MODEL`. Without those, the
sidecar uses the user's Codex config. Live tests may set `AIS_MODEL=gpt-5.6-luna`.

The sidecar listens on `127.0.0.1` with `AIS_PORT` (default `0`) and registers
`prompt.submit`, `transcript.read`, `state.observe`, and `abort`.
