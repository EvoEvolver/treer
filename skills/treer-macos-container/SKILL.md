---
name: treer-macos-container
description: Set up an Apple container machine as a Treer Host on a Mac. Use when installing Treer in Apple Container, creating a container machine, mapping ~/.codex or workspaces into a Linux VM, or when asked for `treer --skill macos-container`.
---

# Treer on an Apple container machine

You are installing a **Linux Treer Host** inside an Apple container machine on
this Mac. Do not enroll the Mac LaunchAgent as the same machine. Do not copy
`~/.config/treer` from macOS into the guest.

This skill is for a human or agent **on the Mac**. It is not the managed-agent
CLI contract (`treer --skill`).

## 1. Preflight — stop if any check fails

```bash
uname -m
sw_vers
command -v container
container system status
```

- Require Apple silicon (`arm64` / `arm64e`). Intel is unsupported.
- Require macOS **26 or later** (`sw_vers -productVersion` major ≥ 26). Apple
  supports `container` on macOS 26; older releases are not a Treer target.
- If `container` is missing: install it (Homebrew `brew install container`, or
  the signed `.pkg` from https://github.com/apple/container/releases), then
  continue.
- If the apiserver is not running: `container system start`. The first start
  may prompt to install a Linux kernel; accept the default.
- Re-run `container system status` and stop if it is not ready.

```bash
container machine list
```

If a machine named `treer` already exists, ask whether to reuse it or pick
another name. Do not delete it unless the user says to.

## 2. Ask before creating anything

Ask **one question at a time** and wait. Do not guess.

**Workspaces.** Apple container machines only offer `--home-mount rw|ro|none`.
`rw` (default) virtiofs-mounts the **entire** macOS `$HOME` at the same path
(`/Users/<name>`). There is no extra `--volume` on `container machine create`.
Subset exposure is done with guest symlinks plus Host `--root`, not by mounting
fewer host directories.

Offer:

1. **Subset (recommended):** `home-mount=rw`. User lists one or more project
   directories (absolute Mac paths). Host `root` is the first of those. Guest
   `/workspace/<basename>` symlinks to each.
2. **Entire home:** `home-mount=rw`. Host `root` is still a directory the user
   names (do not set Host root to `$HOME`). Warn that Agents can walk the
   virtiofs home tree.
3. **Isolated:** `home-mount=none`. No live Mac files. Only use this if the
   user accepts copying trees into the VM disk.

**CLI config** (symlinks from Linux `$HOME` → `/Users/<name>/...`). Probe the
Mac home and list what exists. Default **off** for `~/.ssh` and
`~/.config/treer`. Typical on:

- `~/.codex`
- `~/.claude` and `~/.claude.json`
- `~/.cursor`
- `~/.grok`
- `~/.pi`
- `~/.config/opencode`
- `~/.gitconfig`

Ask which of the existing ones to link. Never link `~/.config/treer` or
`~/.ssh` unless the user explicitly names them.

**Enrollment.** After the machine exists, ask the user to paste the **Add
machine** connect command from the Treer UI (the `TREER_ENROLLMENT_KEY=...
treer-agent-server connect --proxy ...` line). Do not invent a Proxy URL.

## 3. Build the image

Do not push or reuse `local/ubuntu-machine:latest`. Build from this repo:

```bash
container build -t local/treer-machine:latest \
  -f deploy/apple-container-machine/Dockerfile \
  deploy/apple-container-machine
```

The Dockerfile is systemd `/sbin/init` only. Treer binaries come from
`install.sh` later.

## 4. Create the machine

Use the name agreed in step 1 (default `treer`):

```bash
container machine create local/treer-machine:latest \
  --name treer \
  --home-mount rw \
  --cpus 8 \
  --memory 16G
```

For isolated mode pass `--home-mount none`. Memory defaults to half of host RAM
if omitted. `--set-default` only if the user wants this machine as `container
machine run` with no `-n`.

```bash
MAC_USER="$(id -un)"
MAC_HOME="$(eval echo "~$MAC_USER")"
GUEST_HOME="/home/${MAC_USER}"
# Same path as macOS, virtiofs when home-mount is rw:
MAC_HOME_IN_GUEST="$MAC_HOME"
```

Linux `$HOME` is `/home/<user>`. The Mac tree appears at `/Users/<user>`.
CLI tools read `/home/<user>/.codex`, so configs must be symlinked.

Inside the guest (`container machine run -n treer -- ...`):

```bash
# configs chosen in step 2, example:
ln -sfn "$MAC_HOME_IN_GUEST/.codex" "$GUEST_HOME/.codex"

# workspaces chosen in step 2:
sudo mkdir -p /workspace
sudo chown "$MAC_USER" /workspace
ln -sfn "$MAC_HOME_IN_GUEST/dev/myrepo" /workspace/myrepo
```

`export HOME="$GUEST_HOME"` for every guest command that runs Treer. Do not
export `HOME="$MAC_HOME_IN_GUEST"`.

## 5. Install Treer in the guest, then enroll

Public install (no credential):

```bash
container machine run -n treer -- bash -lc \
  "curl -fsSL 'https://PROXY_HOST/install.sh' | sh"
```

Replace `PROXY_HOST` with the Proxy host from the pasted connect command.
This installs `treer` / `treer-agent-server` under the guest user's
`~/.local` (`/home/<user>/.local`), not the Mac's.

Then enroll with the pasted key. Force a **new** machine identity. Guest
hostname is the container machine name (`treer`), so it will not reuse the
Mac's `Mac.home.com` identity.

```bash
container machine run -n treer -- bash -lc '
set -e
export HOME=/home/'"$MAC_USER"'
export PATH="$HOME/.local/bin:$PATH"
export TREER_WORKSPACE_ROOT=/workspace/myrepo   # first chosen workspace
# paste key + proxy from the UI:
TREER_ENROLLMENT_KEY="enr_v1_..." \
  treer-agent-server connect \
  --proxy "https://PROXY_HOST/" \
  --root "$TREER_WORKSPACE_ROOT" \
  --service-mode nohup \
  --non-interactive --accept-risk \
  --name apple-container
'
```

`--service-mode nohup` avoids requiring a user systemd session in the guest.
Wait until connect prints that Controller and Proxy are ready
(`proxy_connected`). The Host stays detached when `container machine run`
exits, but it is intentionally not restarted after a crash or machine reboot.

## 6. Verify

From the guest:

```bash
hostname
treer-agent-server service status
LISTEN="$(python3 - <<'PY'
import glob, json, os
paths = glob.glob(os.path.expanduser("~/.config/treer/agent-servers/*-controller.json"))
print(json.load(open(paths[0]))["listen"])
PY
)"
curl -sS "http://${LISTEN}/api/health"
```

Success is `proxy_connected` (or equivalent connected lease), **not** a live
loopback API alone. Confirm the controller json `server_id` is **not** a
server id still installed as a Mac LaunchAgent
(`~/Library/LaunchAgents/dev.treer.agent-server.*.plist`).

`container machine run` without a command is a login shell. Leaving that
shell does **not** stop the machine. Stop the Host with
`treer-agent-server service stop` in the guest, or
`container machine stop treer` on the Mac.

## Boundaries

- After a guest reboot, run `treer-agent-server service start`; nohup does not
  provide boot activation or crash restart.
- Do not copy Mac `~/.config/treer/agent-servers` into the guest.
- Do not enroll the same `server_id` on the Mac and in the container.
- Do not set guest `HOME` to `/Users/<name>`.
- Do not claim `home-mount=rw` plus subset symlinks hides the rest of `$HOME`
  from a process that walks `/Users/<name>`. Host `root` is the working-tree
  boundary; virtiofs is still the full home.
- Rebuild the image from the Dockerfile when changing packages. Do not
  treat `container image save` of a local tag as the distribution path.
