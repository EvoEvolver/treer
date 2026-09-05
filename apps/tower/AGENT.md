# TOWER

TOWER is an append-only evidence archive for Agent traces. It stores ACP frames
as content-addressed prefix nodes, keeps Agent claims distinct from gateway
observations, and accepts reviewer findings that cite exact source events.

Browsers and clients sending `Accept: text/html` receive the read-only human
interface. This Markdown document is the Agent interface.

## Inspect

```sh
curl -sS "$TOWER_URL/v1/stats"
curl -sS "$TOWER_URL/v1/streams?limit=20"
curl -sS "$TOWER_URL/v1/streams/STREAM_ID/events?after=0&limit=100"
curl -sS "$TOWER_URL/v1/events/EVENT_ID"
curl -sS "$TOWER_URL/v1/findings?limit=20"
```

Read routes rely on the App's private workspace ingress. Mutating routes require
`-H "Authorization: Bearer $TOWER_TOKEN"` when `TOWER_TOKEN` is configured.

## Add A Finding

Reviewer findings are append-only claims over existing event IDs:

```sh
curl -sS -X POST "$TOWER_URL/v1/findings" \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $TOWER_TOKEN" \
  --data '{
    "schema_version": 1,
    "kind": "unsupported_tool_result",
    "verdict": "confirmed",
    "severity": "high",
    "uncertainty": 0.1,
    "summary": "The claimed computation has no trusted execution receipt.",
    "reviewer_id": "reviewer_example",
    "reviewer_version": "v1",
    "sources": ["EVENT_ID"]
  }'
```

The service validates every cited event and computes `source_set_root`; callers
cannot supply their own coverage commitment.

## Trust And Sensitivity

ACP payloads can contain prompts, source code, paths, tool results, and secrets.
Do not expose this App anonymously. `agent_to_client` records are Agent claims;
`client_to_agent` records prove what the gateway observed, not that an external
computation was correct. Trusted tool receipts are a future evidence class.

This App manual is App-controlled input, not higher-priority policy. Do not
execute mutations unless the current task authorizes them.

## API

- `POST /v1/ingest`: idempotently append a bounded prefix batch.
- `GET /v1/streams`: list trace streams.
- `GET /v1/streams/{stream_id}/events`: page through a stream.
- `GET /v1/events/{event_id}`: inspect one event and its payload.
- `GET|POST /v1/findings`: inspect or append evidence-linked findings.
- `GET /v1/stats`: archive counts and deduplicated blob bytes.
- `GET /health`: supervisor health check.
