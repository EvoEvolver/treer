# Caller-owned actual owner audit browser leaf

`owner-audit-direct.mjs` exports `runOwnerAudit({page,binding})`. It does NOT
launch a browser, create a context/session, log in, attach headers, create tokens,
run allowed/denied operations, or create a fixture. Release owns one fresh actual
stack/current authentic owner context and calls this after its real admitted and
denied operations, before teardown. Do not emulate a signed assertion in the
browser. No production human session is required for a legitimate isolated fixture.

```js
import { runOwnerAudit } from '/absolute/source/apps/external-agent/tests/owner-audit-direct.mjs'
const result = await runOwnerAudit({page: callerOwnedPage, binding: {
  owner_url: 'http://127.0.0.1:PORT/api/workspaces/WS/apps/APP/ui/',
  identity: {workspace:'WS',app:'APP',service:'SERVICE',agent:'RUNTIME'},
  generation: 'ACTUAL_CURRENT_GENERATION',
  ui_sha256: {'settings.js':'EXACT_64_HEX_JS','settings.css':'EXACT_64_HEX_CSS'},
  source_sha: 'EXACT_40_HEX_BUILT_SOURCE',
  artifact_sha256: 'EXACT_64_HEX_APP_ARTIFACT',
  deadline_ms: Date.now()+120000,
  expected_records: [
    {request_id:'ACTUAL_ALLOWED_REQUEST_ID',operation:'transcript',target:'ALLOWED',outcome:'completed'},
    {request_id:'ACTUAL_DENIED_REQUEST_ID',operation:'transcript',target:'DENIED',outcome:'policy_denied'}
  ]
}})
// Write result exclusively to the caller-owned safe receipt. Always own cleanup.
```

HTTPS or literal loopback HTTP only; owner URL must end with the exact scoped
`/api/workspaces/{workspace}/apps/{app}/ui/` prefix. Binding has no credential
field: existing authentic cookie/storage belongs to the supplied context. Source
and artifact hashes are caller provenance bindings, not executable assertions independently
verified by this leaf (served JS/CSS hashes ARE checked); caller must bind them to its actual executable/build.
Expected IDs come from the same fresh real requests, not a static historical row.
Keep the fixed binding in caller memory/private0600 storage, never log the session.

Use a dedicated caller-owned page without competing routes/navigation. Leaf adds
and removes only its own read-only transport guard for that exact origin/prefix;
every allowed response comes from the real server, never fulfilled/mocked. It
navigates directly through the real authenticated App UI tunnel, observes the
loaded identity/current generation/disabled state and actual served JS/CSS response hashes, then manually refreshes twice at1100px and390px, requires real200/
no-store audit JSON <=128KiB/100rows/fullidentity, exact correlated rows/outcomes,
matching rendered row count, masked token IDs, no dynamic links/inputs/images or
mutation controls in audit and no horizontal overflow. It does not mutate the App,
call login/logout or change Policy. Each operation is bounded by supplied deadline
(max5minutes); no retry. Return uses fixed failure stage/response hashes/counts;
never private DOM, bodies, token IDs or diagnostic errors. Browser/context remain
open and caller must revoke/stop/clean up on PASS, FAIL or uncertainty.

Focused controls (synthetic UI HTTP fixture only, not actual authorization proof):

```sh
EXTERNAL_PLAYWRIGHT_MODULE=/absolute/playwright/index.mjs \
  node apps/external-agent/tests/owner-audit-direct-controls.mjs
```

No actual execution is claimed by author checks. Old b1bf artifacts do not include
b6df's embedded UI: Release must use its newly built, source-bound audit artifact.
This leaf is support only; do not rebuild product or re-run old27/Policy30 merely
to integrate it.230 remains separate.
