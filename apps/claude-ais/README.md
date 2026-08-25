# Claude Code AIS

Thin Treer Agent Interface adapter over Claude Code's stream-json session
protocol (`--print --output-format stream-json --input-format stream-json`).
One Treer Agent owns one session. Built-in `--kind claude` remains the TUI
path.

## Launch

```bash
treer agent admin profile create "Claude Code AIS" \
  --description "Claude Code stream-json session with a per-Agent AIS adapter" \
  --cwd treer apps/claude-ais/scripts/treer-agent.sh
treer agent admin profile launch "Claude Code AIS" --name claude-ais
```

Without an Anthropic credential, set `ANTHROPIC_BASE_URL` and
`ANTHROPIC_API_KEY` (or `ANTHROPIC_AUTH_TOKEN`) and `CLAUDE_MODEL` /
`AIS_MODEL`. Live tests reuse the Codex OpenAI-compatible endpoint as
`gpt-5.6-luna` when Claude has no native token.
