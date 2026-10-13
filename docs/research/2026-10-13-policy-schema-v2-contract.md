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
