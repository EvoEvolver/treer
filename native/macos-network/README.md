# Native macOS network backend

This is an **experimental** owned Network Extension and launch helper. It is not
yet a supported replacement for Linux transparent networking. It does not use
the signed Mitmproxy helper or require Python at runtime.

## Account-free verification

Install full Xcode and XcodeGen, then run from the repository:

```sh
python3 scripts/check-macos-network.py --developer-dir /Applications/Xcode-beta.app/Contents/Developer
cargo test -p treer-agent-server network
```

The script runs the Swift transport tests, kernel audit-token/registration tests,
and an unsigned app/extension build. It also checks the embedded extension's
location and provider class. Logs and products remain under ignored `output/`.
It does not sign, install, activate, modify DNS or change routes. Generated
Xcode projects and Swift build caches must not be committed.

The transport suite includes real localhost Network.framework sockets: a binary
1 MiB request, FIN, and a server response that only starts after request EOF.
Callback tests cover the reverse FIN direction, payload with EOF in one read,
backpressure and cancellation. These tests exercise the owned copier; they do
not substitute for an installed NEAppProxyTCPFlow end-to-end test.

## Signed runtime gate

The app identifier is `org.treer.network`; the extension identifier is
`org.treer.network.extension`. Both need matching Network Extension provisioning
profiles and the same signing team. After configuring a capable Apple Developer
team in Xcode, build the generated project with signing enabled. Install the
validly signed app in `/Applications`, then run:

```sh
/Applications/TreerNetwork.app/Contents/MacOS/TreerNetwork activate
/Applications/TreerNetwork.app/Contents/MacOS/TreerNetwork status
```

Activation can require macOS user approval. `activate` requests activation;
only a successful versioned `status` reply acknowledges readiness. To deactivate
this backend, use the same executable with `deactivate`; it selects only Treer's
own provider configuration. It does not remove or alter Tailscale or Mitmproxy.

Run an isolated Controller with `TREER_NETWORK_MODE=native-experimental`.
`TREER_MACOS_NETWORK_HELPER` may name an absolute helper executable path.
The Controller requires a successful readiness reply before startup. Each Agent
executes through the helper, which registers its PID and birth time and waits for
the extension's ACK before replacing itself with the workload. Arguments are
passed directly, without shell interpolation. Existing Host wire models remain
unchanged. The experimental helper currently requires an IPv4 loopback SOCKS
listener. Mac services use shared localhost ports; Linux keeps its namespace
service bridges.

## Current behavior and uncompleted gates

| Boundary | Implemented | Still required |
| --- | --- | --- |
| TCP | Owned bidirectional copier, directional FIN, bounded read pipeline | Installed extension regression and throughput/long-idle tests |
| Identity | OS audit-token validation, PID birth-time checks, registration ACK after atomic checkpoint, ancestry lookup, same-boot live-identity restore | Detached children before first flow; installed crash/restart verification |
| Failure handling | Known managed TCP fails closed if SOCKS setup fails; unsupported managed flows are rejected | Extension death/disable protection and installed sleep/wake tests |
| Controller | Explicit experimental mode, all captured TCP goes through Proxy Open, local API and shared-port ingress | Signed Agent end-to-end tests and startup failure reporting in Agent status |
| DNS | Shared persistent virtual addresses, UDP/TCP responder, exact-domain resolver lifecycle, TCP/UDP reverse mapping | Signed system resolver/capture/cache validation |
| UDP | Owned flow adapter, authenticated framed Controller transport, Direct and cross-Proxy datagrams, active Policy and usage | Installed capture and DNS verification; Linux private Agent UDP ingress |
| Policy and usage | Open and active authorization, relay ledger, durable Direct report replay/ACK and Agent detail | Uncheckpointed crash tails; commit-time bucketing of delayed reports |
| Packaging | Reproducible unsigned app/test build, activate/status/deactivate/uninstall source | Signed install/update/uninstall and recovery validation |

Unregistered traffic is left with macOS. An unknown or detached process is not
automatically recognized as managed. The provider checkpoints roots and observed descendants in its private Application
Support directory (0700 directory, 0600 file). Restore validates the boot session
and each live process birth time. Corrupt or insecure state prevents startup.
Existing connections are not resumed after provider death; restored SOCKS ports
still need their original Controller. Installed recovery remains an acceptance gate.
Do not describe this as a hostile-workload security boundary or full Linux parity.

Design references are Apple's NetworkExtension SDK APIs and the MIT-licensed
Mitmproxy redirector investigation recorded in the
[research report](../../docs/research/2026-09-05-macos-transparent-network.md).
This implementation does not vendor upstream binaries or generated IPC code.

The privileged provider manages only Treer-owned exact-domain files in
`/etc/resolver`; it does not replace global DNS or Tailscale settings. Controller
`sync-hosts --network-proxy URL --hosts-stdin` messages follow accepted snapshots.
Provider death can leave these files until recovery; installed crash cleanup and
uninstall are not verified. `status` reports capability levels explicitly,
including observed-ancestry child coverage and the absence of an extension-death
fail-closed guarantee. `uninstall` removes its own manager configuration and asks
macOS to deactivate the extension; it does not remove the app bundle itself.
