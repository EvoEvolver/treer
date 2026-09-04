# OpenCode AIS

Compatibility sidecar. Prefer **New → OpenCode thread** in the Treer control
plane (`kind=acp`), which starts `treer-acp --harness opencode` and the Host
thread UI.

Thin Treer Agent Interface adapter over `opencode serve`. One Treer Agent owns
one OpenCode session. A TUI-only `opencode` process is not this adapter.

## Launch

```bash
treer agent admin profile create "OpenCode AIS" \
  --description "OpenCode HTTP with a per-Agent AIS adapter" \
  --cwd treer apps/opencode-ais/scripts/treer-agent.sh
treer agent admin profile launch "OpenCode AIS" --name opencode-ais
```

If OpenCode has no native provider, set `OPENAI_BASE_URL`, `OPENAI_API_KEY`,
and `AIS_MODEL` (live tests use `gpt-5.6-luna`).
