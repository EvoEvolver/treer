import { useSyncExternalStore } from "react";

export type ThemeMode = "system" | "light" | "dark";
export interface AppearanceState { theme: "light" | "dark"; mode: ThemeMode }
interface Boot { get(): AppearanceState; setTheme(value: ThemeMode): void; subscribe(listener: (state: AppearanceState) => void): () => void }
declare global { interface Window { TreerAppearance: Boot } }

// boot.js (loaded from <head>) owns the choice; React only reflects it.
const boot = window.TreerAppearance;
let current = boot.get();
const subscribers = new Set<() => void>();
boot.subscribe((state) => { current = state; subscribers.forEach((notify) => notify()); });

export function useAppearance(): AppearanceState {
  return useSyncExternalStore((notify) => { subscribers.add(notify); return () => { subscribers.delete(notify); }; }, () => current);
}
export const setThemeMode = (mode: ThemeMode) => boot.setTheme(mode);
