# Release engineering trust-boundary review

Scope: commits after `1a8639b` through `222664d`, reviewed from source and focused
closed tests on 2026-10-12. This is a cross-review record, not permission to
deploy, update an enrolled machine, or restart a service. Later launcher and
Soul implementation commits require an addendum when merged.

| Change | Boundary review | Finding |
| --- | --- | --- |
| `c863b77` email endpoint identity handling | Authenticated human identity, CSRF, credentials | An explicitly supplied empty identity header fails verification instead of falling back to a browser session. Cross-site mutation rejects `Sec-Fetch-Site: cross-site` and requires an allowed Origin. App calls still use verified service-audience identities, not copied workload credentials. No blocker found in this diff. |
| `3c9bf68` service/workload environment | Host/Controller startup, credentials, runtime authority, Proxy routing | Entry-point cleanup runs before Tokio threads; Unix re-exec preserves PID while removing inherited identity from the initial environment. Host-to-Controller, nohup, foreground and Windows launches clear then explicitly filter environment. PTY runtime clears inherited values before applying the new workload's explicit environment. Removed values are never logged. Control-plane and startup-validation HTTP clients bypass inherited proxies; startup validation remains authenticated and retries rather than starting unvalidated Agents. No blocker found. |
| `0a099e9` Soul API contract | Auth, ownership, immutable snapshots | Documentation only: requires authenticated service-audience identity, principal-derived ownership, immutable snapshots and no unauthenticated archive exception. This does not demonstrate implementation enforcement; review server/client changes after merge. |
| `b81c54e` prompt delivery contract | Wire models, queue identity, completion delivery | Adds shared protocol models and documentation, with wire tests; no handler or permission implementation in this commit. Queue ownership, receiver authorization and durable notification execution remain implementation review gates. |
| `222664d` AIS App retirement | Removed runtime/API entry points | Removes duplicate Apps and old shared AIS implementation. No new credential delegation or route is introduced. Dependent infrastructure still needs to stop importing removed `apps/ais-kit/provider-env.mjs` and invoking removed tests; release engineering owns that follow-up. Saved-entry-point compatibility must be checked with the single-launcher merge. |

For `3c9bf68`, `service` subcommands preserve their managed-call context so the
Host-stop warning still works; only daemon/run entry points clean themselves.
The environment filter removes known Treer workload/operator identity, Agent UI
state and loopback HTTP(S)/ALL proxy settings. It deliberately retains normal
host configuration and explicitly supplied new workload settings. It is not a
general secret scrubber or a sandbox. An older running Host remains older until
an explicitly coordinated cold restart; a hot Controller update alone does not
retroactively change that Host's environment behavior. Windows process cleanup
has a separate non-exec implementation and still requires native acceptance.

Validation evidence: inspected shared environment unit tests, the real-service
subprocess fixture and PTY environment tests added by `3c9bf68`; reviewed the
changed client authentication/routing code and email endpoint tests. No enrolled
machine was used for acceptance. New machine-update tooling is tested against
fake Controller/CLI binaries and loopback HTTP only, including version/checksum
checks, activation order, PID/generation preservation, detachment and rollback.
It requires a separate Core review before merge because it controls startup.

Known operational limits: cold rollback restores software, not terminated
processes; checksum equality against Canary HTTPS is not signed release-channel
verification; local shared binaries serving multiple workspaces require a
coordinated update rather than a single-workspace script invocation. The signed
release-channel updater remains the ordinary distribution path.
