# Optional goal budgets (issue #14)

Previously GoalGuard implicitly applied 100000 tokens and 20 rounds. Null token
budgets were converted to undefined and selected the same defaults, despite the
UI describing them as optional. The round limit was not editable in the UI.

Budgets now default to null (unlimited), not Infinity or an arbitrary large
number. Positive installation environment defaults remain opt-in; explicit null
overrides them. New objectives use supplied budgets or the opt-in defaults;
omitting a field on a budget/status patch retains the current value. The API
accepts null for either budget to remove it. Codex receives null tokenBudget via
thread/goal/set; the launcher owns round limits. Claude owns no native token cap,
so the wrapper enforces only explicitly finite budgets. No-progress rejection
protection remains separate and enabled by default.

The UI shows Unlimited/无限制, with blank editable token/round fields and an
explanation. Finite budgets have visible values. Goal-edit prompts tell the
model “unlimited” rather than the ambiguous “default”.

Resume remains native-history authoritative. Missing/null checkpoint budgets
normalize to null, including during the existing legacy usage-epoch reset; that
reset never invents new caps. Saved numeric values (even 100000/20) are retained:
old records lack provenance, so matching the former default is not evidence that
the user omitted it. The UI can explicitly clear these. Native cleared goals
must not be resurrected, and verified cumulative-usage baselines remain intact.

Checks use hermetic fake Codex/Claude sessions, unlimited usage beyond the old
cap, finite-limit retention/explicit removal/resume, missing/null/numeric legacy
checkpoints, environment overrides and independent no-progress detection. The
browser suite covers removal of a finite cap and English/Chinese unlimited UI.
No real-provider inference, native Windows test or runtime restart is required
or performed for this change.
