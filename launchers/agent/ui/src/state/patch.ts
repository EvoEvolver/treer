import type { Snapshot } from "../types";

// Decodes a WebSocket `patch` frame (launchers/agent/src/state_patch.rs) against the frame the same socket delivered
// before it. Fields named in `same` are copied; inside `thread`, a string is the unchanged turn (or, inside a turn, item)
// with that id. Unchanged turns keep their previous object identity. Throws when a reference cannot be resolved.
type Entry = { id?: unknown } & Record<string, unknown>;
function firstById(values: unknown): Map<string, Entry> {
  const map = new Map<string, Entry>();
  if (Array.isArray(values)) for (const value of values as Entry[]) if (typeof value?.id === "string" && !map.has(value.id)) map.set(value.id, value);
  return map;
}
function resolve(values: unknown, previous: unknown, inner?: string): unknown {
  if (!Array.isArray(values)) return values;
  const before = firstById(previous);
  return values.map((value: unknown) => {
    if (typeof value === "string") {
      const old = before.get(value);
      if (!old) throw new Error("patch references an unknown entry");
      return old;
    }
    const entry = value as Entry;
    const old = inner && typeof entry?.id === "string" ? before.get(entry.id) : undefined;
    return old && inner ? { ...entry, [inner]: resolve(entry[inner], old[inner]) } : entry;
  });
}
export function applyPatch(base: Snapshot, patch: Record<string, unknown>): Snapshot {
  const previous = base as unknown as Record<string, unknown>;
  const out: Record<string, unknown> = {};
  for (const key of (patch.same as string[] | undefined) ?? []) out[key] = previous[key];
  for (const [key, value] of Object.entries(patch)) {
    if (key === "type" || key === "same") continue;
    const old = previous[key] as Entry | null | undefined;
    const thread = value as Entry | null;
    out[key] = key === "thread" && thread && typeof thread === "object" && old && old.id === thread.id
      ? { ...thread, turns: resolve(thread.turns, old.turns, "items") } : value;
  }
  out.type = "state";
  return out as unknown as Snapshot;
}
