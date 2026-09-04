# Grok Build AIS

Compatibility sidecar. Prefer **New → Grok thread** in the Treer control plane
(`kind=acp`), which starts `treer-acp --harness grok` and the Host thread UI.

Thin Treer Agent Interface adapter over Grok Build's Agent Client Protocol
server (`grok agent stdio`). One Treer Agent owns one ACP session. Built-in
`--kind shell` remains the TUI path if you launch `grok` interactively.

Grok Build does not ship a Codex-style app-server. ACP over stdio is the
first-party integration surface for editors and orchestrators.

## Launch

```bash
treer agent admin profile create "Grok Build AIS" \
  --description "Grok Build ACP session with a per-Agent AIS adapter" \
  --cwd treer apps/grok-ais/scripts/treer-agent.sh
treer agent admin profile launch "Grok Build AIS" --name grok-ais
```

Uses the local `grok` login or `XAI_API_KEY`. Optional model override:
`GROK_AIS_MODEL` or `GROK_MODEL`. Do not point this sidecar at `agent`; that
name is also used by Cursor CLI on some installs.

The sidecar listens on `127.0.0.1` with `AIS_PORT` (default `0`) and registers
`prompt.submit`, `transcript.read`, `state.observe`, and `abort`.
