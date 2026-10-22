# HOME cwd and Controller private placement (#158)

The launcher follows the existing Controller `private_directory_placement`
contract in `crates/treer-agent-server/src/private_state.rs`. Canonical private
state outside cwd is valid; below cwd it must begin with a hidden component.
An empty relative path (private==cwd) or ordinary first component is refused.
No VCS marker of any type may exist at the private directory or any ancestor.
Physical no-follow paths, owner/mode checks, single-writer lease and per-write
placement rechecks remain required. There is no cwd fallback or state relocation.

This fixes leader's HOME cwd compatibility: `/Users/dooms/.local/state/...` is
hidden OS state under `/Users/dooms`, already admitted by the Controller. The
previous blanket `root.starts_with(cwd)` refusal exited before acquiring a lease
or importing Node history. Managed startup still requires the bound Controller
private-state status; standalone fixtures remain explicit private-root only.

Hidden placement is a repository-leak prevention rule, not an OS sandbox or a
claim that authenticated workspace-file routes cannot access same-account
files. File browsing behavior is unchanged from Node. A new VCS marker disables
future private writes even after successful startup. Changing cwd, loosening
VCS/symlink/ownership checks or relocating Controller state is not a remedy.

The focused actual-entry test imports a synthetic Node session under HOME cwd,
stores receipt/tool history in a hidden private tree, restarts the same session
without replay, and verifies equal/ordinary-cwd and VCS placements refuse before
provider spawn. No live Agent, provider-native history or credentials are used.
