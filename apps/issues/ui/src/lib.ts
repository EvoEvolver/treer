import type { Principal, Priority, Status } from "./types";

export const STATUSES: Status[] = ["open", "in_progress", "blocked", "done", "closed", "note"];
export const PRIORITIES: Priority[] = ["low", "normal", "high", "urgent"];
export const PRIORITY_GLYPH: Record<Priority, string> = { low: "▂", normal: "▃", high: "▅", urgent: "!" };
export const COLLAPSED_BY_DEFAULT: Status[] = ["done", "closed", "note"];

export const words = (value: string) => value.replaceAll("_", " ");

export const principalContext = (person: Principal) =>
  person.kind === "agent" ? person.path || person.id : "Human";

export function displayName(people: Principal[], id: string | null): string {
  const person = people.find((p) => p.id === id);
  return person ? `${person.name} · ${principalContext(person)}` : id || "Unassigned";
}

export function issueFromHash(): number | null {
  const id = new URLSearchParams(location.hash.slice(1)).get("issue");
  return id && /^\d+$/.test(id) ? Number(id) : null;
}
