# Agent targets are workspace tree paths

Agent and App runtime labels are case-sensitive and unique within their tree
folder; Policy and lifecycle transport continue to use stable IDs. Relative
paths start at the caller Agent's containing folder, or `/` for an operator.
Bare labels resolve only among siblings. Missing sibling names return
`agent_not_in_folder` with at most ten matching absolute paths in `hint`, never
a global-name fallback. `self` and `.` select the caller when an Agent is
required; folder commands retain their existing `.` folder meaning. `..` at
root stays at root. Copy path produces a directly usable absolute target.

Resolution uses one shared pure tree resolver for CLI and Message recipients.
The Controller resolves targets before issuing ID-addressed API operations.
Human Message recipients retain their existing stable-ID/display-name lookup;
Agent labels only use sibling lookup. Profiles and machines keep their existing
separate namespaces: profile launch's profile argument is not an Agent target.

Creation reserves the name and placement together before dispatching any process
start. Rename, move, and folder deletion that promotes children serialize on the
same workspace row and reject sibling collisions. Existing globally unique
labels need no destructive data migration. Untracked legacy runtime names are
backfilled into durable metadata when inventory is received. New workload labels
cannot be path separators or reserved self/dot names; legacy entries remain
addressable by ID. App display labels are independent of their owned service
names; service/virtual-host names retain workspace-wide uniqueness. When an
App reuses a label whose owned service name is taken, its new service gets a
stable-ID suffix rather than adopting or renaming the existing service. The
browser also submits final placement in the create/launch request, instead of
creating temporarily at root and moving afterward.

Paths in inventory are derived from the current tree, never trusted as process
identity. Message path fields are immutable display snapshots alongside the
existing name snapshots. IDs remain authoritative after moves/renames.

The new CLI checks the Controller's advertised `agent.path-targets.v1` capability
before dispatch. A missing capability returns a clear upgrade error rather than
allowing an older parser to silently apply global-name semantics. This deliberate
client compatibility gate also protects bare sibling targets, not just strings
containing `/`; it does not restart or upgrade a Controller automatically.

Validation uses pure resolver tests, isolated Controller/Proxy HTTP fixtures,
PostgreSQL folder/create/rename/move collision tests, browser Copy path tests,
CLI help/compatibility tests, and maintained-doc checks. No live workload or
shared database is mutated for verification.
