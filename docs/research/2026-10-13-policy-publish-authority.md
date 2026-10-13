# Policy publication authority

This replaces the abandoned delegation/grant proposal. Core stores one owner
choice on the selected Provider binding: `owner_session` (default) or
`provider_operator`. Policy mode/default decisions remain unchanged. Provider
selection and owner rescue remain owner-only; the choice grants no new authority
to change either. There are no grants, expiry, protected-action or scope checks.

## Contract: policy.publish-authority.v1

`WorkspacePolicyProvider.publish_authority` is additive, defaulting to
`owner_session` when absent. New clients must not use the legacy Provider PUT to
change it: old servers ignore unknown fields. Use the dedicated endpoint and
require the capability in its response; 404 or a missing capability is an
explicit upgrade error, never permission to publish.

- `PUT /api/workspaces/{workspace}/policy-provider/publish-authority`: current
  owner session only. JSON: `{"expected_app_id":"app_...",
  "publish_authority":"provider_operator"}`. The expected App must still be the
  selected Provider; a different selected App fails. This is not a revision CAS
  and does not detect switching away and back to the same App. Response contains `provider` and
  `capabilities:["policy.publish-authority.v1"]`.
- `GET /api/apps/{service}/policy/publish-authority`: selected Provider runtime
  workload identity, returning `provider`, `publish_authority`, capabilities and
  supported schema versions. No owner token is required for this read.
- `PUT` on that App endpoint additionally requires a current, service-audience
  owner token in `X-Treer-Identity-Token`; same request/response as the owner PUT.
- `POST /api/apps/{service}/policy/operator/validate`: selected Provider runtime
  identity; JSON is the candidate `WorkspacePolicyDocument`. Every call reads
  the current binding from Postgres and requires `provider_operator`.
- `POST /api/apps/{service}/policy/operator/explain`: same authentication and
  authority check; same input/output as existing owner explain, including
  `{subject:{kind:"agent",path:"/team/editor"},action:"agent.prompt",
  resource:{kind:"agent",path:"/team/worker"},document:{...},mode:"monitor"}`.
  It does not expose decision logs or change rules.

Runtime identity uses `X-Treer-Agent-Id` and
`X-Treer-Workload-Credential`, freshly checked against the selected App's bound
runtime. Core never accepts, receives or logs the Policy App management token.
The App authenticates that token locally, then calls the operator endpoint before
publishing schema v2. Existing owner validate/explain/decision endpoints retain
their dual-proof owner requirement, including in operator mode.

## Persistence, races and compatibility

The authority defaults in SQL, and a database trigger resets it to
`owner_session` when the Provider App/service changes, including writes from old
Proxy binaries. Switching authority and writing the audit event are one
transaction, with an expected-App check. Reads are not cached. A previously
validated App write can complete concurrently with a switch back; this is not
instantaneous cancellation of in-flight App operations.

Upgrade all Proxy replicas before enabling operator mode. Old replicas return
404 for the dedicated operator endpoints and keep requiring owner on the legacy
validator; clients must fail clearly, not retry against legacy owner paths or
commit unvalidated changes. A capability returned by one replica is not proof
that the entire fleet was upgraded. Older App versions do not gain token-based
v2 publication until updated. No Controller protocol change is necessary for
these end-to-end HTTP APIs.

## Trust and isolation

Choosing operator mode trusts the selected Provider App and anyone holding its
management token to edit policy, including rules governing themselves. Core
does not constrain their rule scope or prevent self-escalation. A compromised
Provider runtime can call the operator APIs directly: Core cannot establish
whether the App checked its private token. Even the default does not make a
malicious Provider trustworthy: it already supplies evaluated bundles.

Use a separate folder for policy-managing Agents and reviewed path rules to
restrict App access. Run the App and token holders with suitable independent OS
users or containers. Files with mode 0600 do not isolate Agents sharing the same
OS user. Owner Provider/Tree recovery remains independent of supplied rules.
