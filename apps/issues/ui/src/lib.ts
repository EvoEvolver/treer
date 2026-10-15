import type { Issue, Principal, Priority, Status } from "./types";

export const STATUSES: Status[] = ["open", "in_progress", "blocked", "done", "closed"];
export const PRIORITIES: Priority[] = ["low", "normal", "high", "urgent"];
export const PRIORITY_GLYPH: Record<Priority, string> = { low: "▂", normal: "▃", high: "▅", urgent: "!" };
export const COLLAPSED_BY_DEFAULT: Status[] = ["done", "closed"];
export const NOT_ALLOWED = "Only the creator, assignee, lease holder or an admin can change this issue";

export const words = (value: string) => value.replaceAll("_", " ");

export const principalContext = (person: Principal) =>
  person.kind === "agent" ? person.path || `Path unavailable · ${person.id}` : "Human";

export function displayName(people: Principal[], id: string | null): string {
  const person = people.find((p) => p.id === id);
  return person ? `${person.name} · ${principalContext(person)}` : id || "Unassigned";
}

export function issueFromHash(): number | null {
  const id = new URLSearchParams(location.hash.slice(1)).get("issue");
  return id && /^\d+$/.test(id) ? Number(id) : null;
}

// Mirrors the backend rule (Store.editable): a live lease overrides everything, so only its holder may mutate.
export function canEdit(me: Principal | null, issue: Issue): boolean {
  if (!me) return false;
  if (issue.claim) return issue.claim.holder === me.id;
  return me.id === issue.creator || me.id === issue.assignee || (me.kind === "human" && ["owner", "admin"].includes(me.role ?? ""));
}
