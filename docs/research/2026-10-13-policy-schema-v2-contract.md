# Policy schema v2 implementation contract

Status: fixed implementation draft for Core/Policy App coordination, not deployed.
Provider envelope stays `treer.policy-provider/v1`; advertise `policy.bundle.v2`
when returning schema 2. A v1-only Proxy rejects schema 2, never drops its conditions.
New Core advertises `policy.path-rules.v2`; Controller transport must advertise the
same capability before Core accepts a v2 bundle fetched from that machine.
The bundle request may add `max_schema_version=2`; old Providers can ignore it and
return v1. A Provider cannot safely downgrade path rules by dropping fields.

Example complete document (inside the existing bundle `document`):

```json
{
  "schema_version": 2,
  "defaults": {"app.use":"deny", "email.send":"deny"},
  "groups": {},
  "rules": [
    {
      "id":"team-b-email", "priority":100, "effect":"allow",
      "subjects":[{"kind":"agent", "tree":{"kind":"agent", "path_glob":"/teamB/*"}}],
      "actions":["app.use", "email.send"],
      "resources":[{"kind":"app", "tree":{"path_eq":"/infra/email-endpoint"}}]
    },
    {
      "id":"local-prompts", "priority":100, "effect":"allow",
      "subjects":[{"kind":"agent"}], "actions":["agent.prompt"],
      "resources":[{"kind":"agent"}],
      "bindings":{"team":{"ref":"subject.tree.folder"}},
      "conditions":[{"op":"resource_below_folder", "folder":{"var":"team"}, "strict":false}]
    }
  ]
}
```

The example App actions are stage 4 vocabulary, not a claim that App checks are
already implemented in stages 1–3. Unknown valid action strings remain supported.

`tree` on subject/resource selectors is optional, with optional AND fields:
`kind`, `name`, `folder_id`, and at most one of `path_eq`, `path_glob`,
`path_prefix:{"path":"/infra","include_self":true}`. `kind` is agent/app/folder.
Paths are canonical absolute, case-sensitive; `*` is one whole component,
one terminal `**` is zero or more. No regex, character classes or interpolation.
Use path_eq for literal wildcard labels. Prefix matching uses component boundaries.

Rule `bindings` defaults to `{}`, `conditions` to `[]`. Bindings values are
exactly one of `{"ref":"subject.tree.folder"}` / resource equivalent,
`{"id":"folder_stable_id"}`, `{"path":"/team"}`. Variables may only be used in
conditions as `{"var":"name"}`; binding chains/cycles are rejected.
Conditions are tagged by `op`: `same_folder` (no extra fields),
`resource_below_subject_folder` (optional strict:false), or
`resource_below_folder` (required typed folder reference, optional strict:false).
Conditions are AND; selector lists remain OR; missing Tree facts do not match.

Schema 1 rejects any v2 fields; new Core continues reading v1 unchanged.
Priority descending, Deny before Allow at ties, first matching rule; schema 2
adds stable rule-ID ordering at same priority/effect. Defaults remain unchanged:
unmatched action Allow; monitor returns Allow while logging computed Deny.
Document limit remains serialized UTF-8 256 KiB and 1000 rules; rule bindings≤8,
conditions≤32; paths≤4096 bytes/128 components. Server validates all unknown
fields/types/groups/variables and path shapes before compiling.

Tree context is loaded by Proxy, not included in the bundle: node_id/kind/name/path,
folder_id/folder_path, ancestor_ids/ancestor_paths, placed, tree_revision.
Explain: owner-only POST `/api/workspaces/{id}/policy/explain` with
`subject:{kind,id}`, `action`, `resource:{kind,id}`. No client Tree/attributes.
Monitor logs are bounded and owner-only; exact diagnostic response will be
documented with the implementation. Owner Provider/Tree rescue is static Core,
subject to valid current membership/workspace and tree invariants, not rules.

## Provider diagnostics bridge (implementation contract)

The configured Provider may use POST `/api/apps/{service_id}/policy/explain`,
GET `/api/apps/{service_id}/policy/decisions`, and POST
`/api/apps/{service_id}/policy/validate`. All three are read/simulation operations:
validate never installs a bundle. Each request requires **both** the Provider's
current runtime credential (`X-Treer-Agent-Id`, `X-Treer-Workload-Credential`)
and the current human's App-audience token (`X-Treer-Identity-Token`). Core checks
the registered Provider binding, active runtime credential (without the ordinary
credential cache), exact service audience, token signature/expiry/workspace,
active service, and current human owner membership. An App's own credential
alone, another App, a non-owner, or an Agent token cannot use this bridge.
It grants no write authority; operator capabilities remain separate. Do not log
either credential/token. The App must protect its own browser diagnostics routes
against cross-origin requests, and forward the caller token only server-side.

Explain input (each reference has exactly one `id` or `path`):

```json
{
  "subject": {"kind":"agent","path":"/teamB/reviewer"},
  "action":"agent.prompt",
  "resource": {"kind":"agent","path":"/teamB/helper"},
  "document": {"schema_version":2,"defaults":{},"groups":{},"rules":[]},
  "mode":"monitor"
}
```

`document` and `mode` are optional candidate simulation; no installation or
command occurs. Paths resolve actual current nodes; hypothetical nonexistent
nodes are rejected. Human subjects use stable member IDs. Service subjects use
`kind:"service"` and a service ID or App path. Resources accept agent/app/folder
and `agent.tree` with the workspace ID. Unknown fields, client Tree facts and
attributes are rejected. App-defined actions are candidates only until stage 4.
Reply: `explanation` includes computed_effect/effective_decision/mode,
matched_rule_id/reason/policy_revision/tree_revision/source;
`recovery_guaranteed:true` and `capabilities:["policy.path-rules.v2"]`.
Owner Tree rescue is reported as source `owner_break_glass` independently of
the supplied candidate; Provider configuration routes are already outside
delegated policy. A syntax-valid candidate can still deny non-recovery actions.

Validate body is the bare document. Reply: valid/schema_version/capabilities,
recovery_guaranteed and warnings. Decision GET reply is
`{decisions:[...],scope:"this_proxy",retention:"bounded_in_memory"}`:
latest 100 entries, newest first, from a 2048-entry per-Proxy ring, filtered by
workspace. No cursor or durable/global audit guarantee in stage 3. Records only
contain IDs, action, time, revisions and explanation for monitor would-deny;
never prompt/file bodies, request attributes or credentials. These are diagnostic
logs, not a tamper-proof audit trail. All diagnostic responses use no-store.
Browser/operator owner routes remain under `/api/workspaces/{id}/policy/*`.
GET policy-provider advertises supported_schema_versions `[1,2]` and capability
`policy.path-rules.v2`; new fetches send `max_schema_version=2`.
