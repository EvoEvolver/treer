# Machine services execution plan

- Status: completed
- Started: 2026-08-18

## Goal

Model long-running services as durable machine resources. Managed agents may
register and maintain them, but an agent exiting must not remove the service or
its workspace hostname.

## Decisions

- `MachineService` owns the destination machine, target host, target port, and
  application protocol.
- `VirtualNetworkHost` is an alias that references one service by ID. Policy
  remains a separate authorization layer.
- Existing virtual-host rows migrate automatically to one service per hostname.
- Service health is an explicit on-demand host-network TCP probe executed by the
  destination Controller. It is not inferred from machine connectivity.
- Treer registers existing long-running services in this change. Starting and
  supervising systemd, Docker, or arbitrary daemons is a later Host capability.

## Delivery

1. Add shared service contracts and SQLite migration.
2. Add browser and managed-agent service CRUD and probe APIs.
3. Resolve virtual-host routing through services.
4. Add CLI and minimal web controls.
5. Update maintained docs and the embedded Agent skill.
6. Verify migration, policy context, routing, Linux behavior, and the full gate.

## Result

- Proxy SQLite now owns durable machine-service records; aliases reference
  services by stable ID and legacy aliases migrate automatically.
- Browser users and managed Agents can register, update, probe, and delete
  services independently from alias management.
- Health probes execute from the destination Controller's host network. HTTP
  browser tunnels reject services registered as raw TCP.
- The CLI and responsive Network view expose the complete workflow.
- Workspace tests, Clippy, documentation checks, web builds, Linux probe tests,
  and desktop/mobile browser geometry checks pass.
