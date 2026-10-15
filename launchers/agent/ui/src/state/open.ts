import { useSyncExternalStore } from "react";

// Open/closed state of activity groups and tool rows, keyed by "turnId:itemId". It lives outside React state so a toggle
// re-renders only the one component that reads that key, and it survives every snapshot update. Keys are server strings,
// so the map has no prototype.
const overrides = new Map<string, boolean>();
const listeners = new Map<string, Set<() => void>>();

export function useOpen(key: string, fallback: boolean): [boolean, () => void] {
  const stored = useSyncExternalStore(
    (notify) => {
      let set = listeners.get(key);
      if (!set) { set = new Set(); listeners.set(key, set); }
      set.add(notify);
      return () => { set.delete(notify); };
    },
    () => overrides.get(key),
  );
  const open = stored ?? fallback;
  return [open, () => { overrides.set(key, !open); listeners.get(key)?.forEach((notify) => notify()); }];
}
