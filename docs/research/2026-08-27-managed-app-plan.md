# Managed App execution plan

- Status: implemented
- Started: 2026-08-27

## Goal

Separate long-running workspace Apps from interactive Agent sessions while
reusing Treer's machine runtime, sandbox, workload identity, service catalog,
and virtual-host routing.

## First vertical slice

- `AppDeployment` is a durable Proxy resource with a desired running state,
  machine placement, command, one HTTP UI port, and one stable virtual host.
- Creating an App atomically reserves its service and virtual host.
- Start, stop, restart, and delete are explicit App operations. A running App
  is reconciled after its Controller reconnects.
- The initial runtime adapter uses a hidden `kind=app` command workload. It is
  excluded from Agent discovery and can later move to a pipe-based Host
  supervisor without changing the App API.
- App commands and arguments are plaintext deployment configuration and cannot
  contain secrets. Secret delivery, installers, multiple endpoints, replicas,
  and placement scheduling are outside this slice.

## Delivery

1. Add shared App deployment contracts and PostgreSQL schema.
2. Add transactional deployment/service/virtual-host persistence.
3. Add Proxy lifecycle APIs and reconnect reconciliation.
4. Add local Controller forwarding and CLI commands.
5. Add a standalone Apps view with lifecycle controls and UI launch.
6. Update architecture, security, App, quality, skill, and operator docs.
7. Verify database lifecycle, Controller commands, routing, CLI parsing, browser
   workflows, workspace tests, and Clippy.

## Follow-up boundary

The Host protocol still models every process as a PTY. A later compatibility
revision should add generic supervised workloads with pipe logs, persisted
restart policy, health checks, backoff, resource limits, and local secret
references. `AppDeployment` remains the control-plane contract across that
change.
