# Enrolled-machine recovery after reboot (#226)

Investigation starts from installed source `42d00a35a6c7c02baf725c2f62a8ff883a9f0bb5`.
The incident reports missing Host socket on two Controller-only restart attempts,
then Controller/Proxy connectivity followed by `403 policy_provider_unavailable`
on explicit runtime restore. New enrollment later succeeding does not establish
that the previous enrollment credential was invalid.

## Boundaries and correction

A nohup Host does not survive an OS reboot. Controller-only restart requires a
live Host; the CLI now refuses a missing/stale socket with the explicit existing-
identity `restore --workspace` command, before spawning a restart subprocess.
An unreachable socket is not proof that the Host stopped: if it is still running,
the diagnostic directs checking socket access instead of starting another Host.
It does not start a second Host, rotate credentials or enroll again.

Restore uses one loaded installed configuration for both cold Host startup and
the request. Connection readiness is distinct from runtime recovery. The latter
still crosses the authenticated local API and Proxy `runtime.restore` Policy
authorization. A typed 403 `policy_provider_unavailable` now explains that
recovery was not authorized, preserves enrollment, and directs an explicit retry
after the configured provider is restored. It does not retry the POST, override
Policy, treat arbitrary denials as readiness errors, or declare workloads healthy.
Successful restore continues to print the per-workload report, including failures.

The websocket startup-declaration validation path in `proxy.rs` uses exact
machine/workspace authentication and bounded persisted IDs. Proxy
`validate_agent_startup` has no PolicyEngine dependency and retains stopped,
revoked and deleted intent. Policy local startup itself has no synchronous
Proxy authorization call. Existing owner-only default-provider installation is
not an existing-provider cold-boot bypass: it rejects an already configured
binding. No new bootstrap privilege is introduced.

## Evidence and limits

The system incident report establishes an OS reboot. A fixed-label-only scan of
the old machine log in the 18:12 UTC minute found two Proxy connections and 25
`restored Agent startup command` entries, with none of the known startup-validation
or command-restore failure labels. No keys, installed private configuration or
arbitrary diagnostic payload were read. These counts do not identify which
workload was the Policy provider or prove its readiness at the failing request.
The same-machine Policy dependency cycle remains unproven.

Hermetic tests use generated temporary configurations and a loopback HTTP fixture.
They cover missing/stale sockets without subprocess launch, a Policy-unavailable
response followed by a separately invoked successful restore with byte-identical
saved credentials, and other authorization/upstream failures without retry or
misclassification. Existing recovery tests cover stopped/revoked identity and
startup intent. This fixture does not reproduce the historical provider outage,
prove a live recovery, or authorize changes to the active machine. Exact historical
provider failure cause remains unknown; no live restart, deployment, fail-open,
credential inspection or automatic reenrollment is part of this change.
