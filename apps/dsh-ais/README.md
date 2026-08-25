# DeepSeek Harness AIS

Thin Treer Agent Interface adapter over DeepSeek Harness session APIs. One
Treer Agent owns one session. This does not attach to a shared `dsh web :3080`
UI; it starts a dedicated host (default) or an SDK JSON-RPC runtime.

## Launch

```bash
treer agent admin profile create "DeepSeek Harness AIS" \
  --description "DeepSeek Harness session API with a per-Agent AIS adapter" \
  --cwd treer apps/dsh-ais/scripts/treer-agent.sh
treer agent admin profile launch "DeepSeek Harness AIS" --name dsh-ais
```

`DSH_AIS_TRANSPORT=host` (default) starts `dsh --profile web --host 127.0.0.1`
on an ephemeral port and calls first-party `session.create` / `session.prompt` /
`session.history`. `DSH_AIS_TRANSPORT=sdk` speaks the stdio JSON-RPC SDK.

Without a native DSH credential, set `OPENAI_BASE_URL`, `OPENAI_API_KEY`,
`DSH_PROVIDER`, and `AIS_MODEL`.
