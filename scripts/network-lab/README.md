# Transparent network research probes

These are opt-in development experiments, not a new Controller network mode.
They do not enroll a machine, edit existing workspace policies, or replace
running Treer binaries. The live experiment creates temporary local instances
and configures Policy only in its disposable database. See the [dated report](../../docs/research/2026-09-05-macos-transparent-network.md)
for tested versions, evidence, and unresolved native macOS requirements.

## Apple container machine

Run with the **guest** Python and the installed **Linux** Controller binary.
The scripts must be visible through the machine's existing home mount. Example
for the investigated Mac (adjust paths on another machine):

```sh
container machine run -n treer -- python3 \
  /Users/mac/dev/treer/scripts/network-lab/capture_probe.py \
  --linux-binary /opt/treer/bin/treer-agent-server \
  --output /Users/mac/dev/treer/output/network-research/linux-capture.json

container machine run -n treer -- python3 \
  /Users/mac/dev/treer/scripts/network-lab/linux_services.py \
  --binary /opt/treer/bin/treer-agent-server \
  --output /Users/mac/dev/treer/output/network-research/linux-services.json
```

`capture_probe.py` removes proxy environment variables, starts an isolated
loopback SOCKS5 fixture, and launches ordinary sockets/curl through the real
`sandbox-exec`. The fixture requires an Agent username, distinguishes each
launch, denies two test destinations, and counts both byte directions. It
serves synthetic addresses itself and makes one real HTTPS request to
`https://example.com` with normal certificate validation. Successful output
alone does not pass: the expected SOCKS record must also exist.

The reserved `192.0.2.1` case proves capture of that address, not a live
Controller API call. Likewise fixture allow/deny and counters are **not** live
Treer Policy or persisted usage evidence. Those boundaries have separate Rust
checks in the report.

`linux_services.py` launches two isolated namespaces listening on the same
private TCP port, selects both via their Unix bridges, and exercises one
host-loopback published port. Temporary processes are stopped after the test.

## Native macOS capture candidate

```sh
python3 -m venv output/network-research/venv
output/network-research/venv/bin/python -m pip install \
  -r scripts/network-lab/requirements-macos.txt
output/network-research/venv/bin/python scripts/network-lab/capture_probe.py \
  --macos --output output/network-research/macos-capture.json
```

Use Python 3.12 or newer. On first launch the upstream package installs its
signed **Mitmproxy Redirector.app** in `/Applications` and requests a network
system extension. macOS must allow the extension in System Settings → General
→ Login Items & Extensions → Mitmproxy Redirector, and allow the network
configuration. No certificate is installed and no TLS is decrypted. The test
waits for OS approval; a waiting extension is **not** successful capture.

The probe selects only its waiting child PID, then releases the child to make
its connection. `exec` into curl preserves that PID. Kernel-provided flow PID
selects the fixture Agent identity; raw TCP is spliced into SOCKS. It tests
literal TCP, a denied destination, the reserved address, virtual DNS allow/deny,
and normal HTTPS. `macos_capture.py` shares the native adapter between probes.
Only `*.treer.invalid` receives synthetic DNS answers. Public DNS responses stay
real because macOS shares its resolver cache, even during TTL-0 synthetic replies.
Cached public addresses may arrive at SOCKS as IPs; this does not prove Linux-like
public hostname Policy. It does **not** solve dynamic child registration, private
loopback, real Controller enrollment, or backend failure containment. The
upstream `set_intercept` API has no acknowledged registration barrier; the
short settle interval is only suitable for a probe. These limitations prevent
using this script as a production launcher.

On ordinary completion the active redirector is closed. The installed app and
system-extension registration remain; they can be managed through macOS System
Settings. A second running redirector may create another network configuration,
so finish the first authorization attempt before starting another probe.

## Live native identity, Policy, NATS, and PostgreSQL

Prerequisites: build the three local binaries, run the documented development
PostgreSQL container (`treer-postgres-test`, loopback 55432, test-only
`treer`/`treer` credentials), and have the existing Apple machine running. The
script creates its own temporary NATS container using the pinned image; it does
not connect to an existing NATS broker. Proxy listeners are loopback only.

```sh
cargo build -p treer-agent-host -p treer-agent-server -p treer-proxy
output/network-research/venv/bin/python scripts/network-lab/live_macos.py \
  --machine treer --guest-ip 192.168.64.3 \
  --output output/network-research/macos-live.json
```

Adjust the guest IP from `container machine list`. This starts two actual Mac
Hosts/Controllers and two actual Proxy processes connected through NATS. A
temporary TCP echo service runs in the Apple guest host network, reached by the
second Mac Controller. Two native `command` Agents are created through the real
Treer API. Their Host-reported PIDs/Agent IDs register with the native adapter;
their plain-socket client clears every proxy environment variable.

In the disposable DB, enforce-mode Policy defaults `network.connect` to deny
and allows only the first Agent ID. Both Agents use the same virtual hostname.
The allowed Agent must receive the 18-byte response to its 12-byte binary input;
the other must be rejected, with an actual `policy_denied` log. The first also
checks the real Controller API at the reserved address. After the ledger flush,
SQL must show exactly 12/18 payload bytes in the two directions, with no second
count by the other Proxy. These are machine-level relay counters.

This proves the existing **registered virtual-host** path. Controllers still
run `proxy-env`: public Direct egress still bypasses central Policy/accounting.
The loopback test Proxies disable enrollment/login authentication. The Apple
target is a temporary host-network service, not a newly enrolled guest Host.
Two local Proxies do not constitute a geographic multi-region benchmark.

Logs and results remain under ignored `output/network-research/live-<id>/`.
On success or ordinary failure, the script stops its Agents, Hosts, Controllers,
Proxies, and guest service, drops its disposable DB, and removes its NATS
container. It leaves existing Treer processes/configuration alone.

Reproduce the **known failing** native TCP half-close case separately:

```sh
output/network-research/venv/bin/python scripts/network-lab/live_macos.py \
  --half-close --output output/network-research/macos-half-close.json
```

With the tested signed helper this intentionally exits nonzero: the client sends
12 bytes and closes only its write side, but gets no reply although the Treer
ledger records the returned 18 bytes. Upstream Swift closes both flow directions
at outbound EOF. Keep this failure separate from the successful framed test;
fixing it requires a corrected signed helper, not a Python socket workaround.
The [report](../../docs/research/2026-09-05-macos-transparent-network.md) links the
exact upstream code and records other parity requirements.

## Native utun privilege probe

```sh
cc -Wall -Wextra -Werror -o output/network-research/utun-probe \
  scripts/network-lab/utun_probe.c
output/network-research/utun-probe
```

This creates and immediately closes one automatically allocated `utun` device
if permitted. It never sets interface addresses, routing tables, PF rules, or
DNS. An `EPERM` result means the current process cannot create the tunnel. A
privileged rerun would test device creation only, not transparent routing or
per-Agent identity.

## Owned datagram transport

Run `live_macos.py --datagram --revoke-active --local-target` for two-Proxy/NATS
UDP routing, Policy revocation and machine/Agent accounting. Add
`--direct-datagram` to verify Direct UDP receipt-based reporting, database commit
acknowledgements and outbox removal. These variants use the owned local Controller
contract explicitly and do not claim native OS capture. They create only isolated
lab services/databases and clean them up. Omit `--local-target` only when the Apple
Linux guest is reachable; that changes the echo target, not the capture proof.
