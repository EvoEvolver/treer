# Retired native macOS network backend

Treer no longer ships or builds its macOS Network Extension, transparent capture,
virtual DNS, or UDP adapter. Their source and experimental evidence remain in Git
history before the [core simplification change](../../docs/research/2026-09-30-core-simplification-plan.md).

Use ordinary machine networking and explicitly registered service ports.
Configure proxies, containers, VPNs, and isolation in your own environment or
launcher. The Controller neither installs a network extension nor changes system
resolver files.

## Existing installations

Removing source or upgrading the Controller does not uninstall an already active
extension. Before switching an existing machine to plain networking:

1. Inventory Agent endpoints that relied on private namespaces or virtual names.
   Make them reachable from the Controller and register explicit service ports.
2. If the old signed TreerNetwork application is installed, use that installed
   application's `deactivate` / `uninstall` commands to remove its own provider
   configuration, then verify that the extension is inactive. Consult the old
   build's documentation if macOS requires an approval or a reboot.
3. Check for resolver configuration left by that installation. Remove only files
   positively identified as owned by Treer, after verifying they are no longer
   used; do not alter VPN, Tailscale, or system-wide DNS settings.
4. Start or repair the Controller with `TREER_NETWORK_MODE=plain`. Retired saved
   modes are rejected with migration guidance rather than silently activated.

Treer does not execute these machine-level cleanup steps during an ordinary
source update. Existing installed binaries must remain available until cleanup
is verified. This document does not claim that an installed extension has been
removed or that its prior experimental gates passed.
