# Bootstrap a team on a new machine

Use this when the user has authorized enrolling **that** machine and creating
a team. Existing working machines are not new enrollments: do not reinstall or
restart them as a shortcut. The examples are a checklist, not a promise of a
15-minute setup or a fresh-account test already performed.

## 1. Enroll into the intended workspace

In the trusted Treer Console, select the intended workspace → Add machine.
Run the installation and one-time connect instructions it generates on the
**new target machine**. Use its actual HTTPS Proxy origin, not a guessed canary
or localhost URL. On POSIX the installer has this shape:

```sh
curl -fsSL 'https://proxy.example.test/install.sh' -o /private/treer-install.sh
# Inspect the trusted installer, then execute it locally when authorized.
sh /private/treer-install.sh
```

Choose a private OS account/root for this machine; install scripts and enrolled
processes can use that account's permissions. Windows uses the Console's
PowerShell installer/connect commands and explicit private workspace root.
Do not substitute shell commands on Windows or migrate an existing enrollment
root implicitly. Do not put actual enrollment keys in examples, tickets or
captured transcripts; use the Console's private connect flow on the target.

After connect, from an authorized operator terminal or managed Agent:

```sh
treer machine list
treer interface contract
```

Confirm the expected machine is online and read its root/Controller identity.
A fresh shell outside a managed Agent needs the intended authenticated local
Controller/operator context; never copy another Agent's workload credential.
For several Controllers under one OS user, explicitly select and verify the
intended local Controller before installing profiles. Follow current
`treer-agent-server launcher --help` and the
[launcher installation instructions](../../../launchers/agent/README.md#installation-and-updates);
do not guess that the default loopback port belongs to this workspace.

## 2. Install only the selected launcher/provider

Install trusted Node.js 22+ on the target and identify its absolute executable.
Run locally against the intended Controller:

```sh
treer-agent-server launcher --node /absolute/node install --agent claude --agent codex
treer-agent-server launcher --node /absolute/node status
treer agent admin profile list
```

The installer saves matching portable profiles and the per-user `treer-agent`
entry; it does not create or restart Agents. Ensure that bin directory is on the
Agent's PATH. Install and log into only the provider CLIs the user selected,
using their own login flow; never read or transmit login files. A profile being
present does not prove that its provider executable or account is usable.
Read [Agent integration](agents.md) for ACP and other supported profiles.

## 3. Create folders and role Agents

Inspect existing paths first so this is not a duplicate team. Substitute the
actual machine name/ID and project directory relative to that machine's root.
The following role split is optional; it is not a required Treer governance
model. Each command creates and starts one process/session:

```sh
treer agent ls -R /
treer agent mkdir -p /team/dev
treer agent mkdir -p /team/review
treer agent admin create --machine target-machine --kind command \
  --name leader --folder /team --cwd projects/example -- \
  treer-agent --harness claude
treer agent admin create --machine target-machine --kind command \
  --name developer --folder /team/dev --cwd projects/example -- \
  treer-agent --harness codex
treer agent admin create --machine target-machine --kind command \
  --name reviewer --folder /team/review --cwd projects/example -- \
  treer-agent --harness claude
```

Create only the user-requested count/roles; more developers or a release role
are optional. The project directory must exist on the target before launch.
Default launchers run with unattended/full machine-account authority; folder
placement is not an OS sandbox. Use separate users/containers/VMs for mutually
untrusted roles and configure Policy deliberately, initially in monitor when
evaluating new rules. Do not infer least privilege from role names.

## 4. Verify a prompt round-trip

Inspect the Agent's registration/session and give each its actual task/role.
For a bounded smoke test that does not modify project files:

```sh
treer agent show /team/dev/developer
treer agent wait /team/dev/developer --until idle --timeout 120000
treer agent prompt /team/dev/developer \
  'Reply with READY and your assigned role. Do not change files or run commands.' \
  --wait --until idle --timeout 120000
treer agent read /team/dev/developer --lines 40
```

Check the actual reply and prompt result, not just the returned status. Read
only enough terminal/transcript content for that check; output may contain
sensitive task data. An idle process may still lack provider login or have
failed its last request; a successful prompt plus the expected AIS/UI is the
stronger signal. If the round-trip fails, inspect the chosen Agent/provider,
report the cause and stop; do not reinstall Core or restart peers to hide it.

## 5. Optional: shared ticket board

**If you want a shared ticket board**, deploy the [Issues App](presets.md#minimal-optional-issues-example)
and point participants at it. It is not required for Core Message, Agent
prompts, a folder tree or team health checks. For a new version-directory App,
the sequence is init → stage → bootstrap → app create; deploy is for subsequent
upgrades, not a replacement for first creation. Adopt whatever collaboration
rules the user asks for, rather than imposing this repository's ticket process
on every workspace.

## Pitfalls

- Reuse discovered paths/stable IDs, not a same-name Agent from another folder.
- Use one build target directory per Agent/worktree (`CARGO_TARGET_DIR` can point
  to that worktree's `target/`), check free disk before large builds, and list
  owned cleanup candidates before deleting them.
- Use random/free test ports and hermetic environments. Do not test using an
  inherited live workload credential, provider login or external mail delivery.
- Never use broad process-name/pkill patterns. Stop only processes you own and
  whose termination the task authorizes. Setup is not permission to upgrade or
  restart a Host, Controller or someone else's Agent.
- An authorized Agent restart may need a fresh task/continue prompt. Verify a
  successful prompt round-trip and idle afterward; `working` alone is not proof
  of readiness, and idle alone is not proof that the task succeeded.
