import { createContext, useContext } from "react";
import type { Issue, MutationResult, Principal, ToastAction } from "./types";

export interface AppContext {
  me: Principal | null;
  people: Principal[];
  busy: boolean;
  /** Runs a write; shows the outcome as a toast and resolves null on failure. */
  mutate(path: string, method: string, body?: unknown, options?: { quiet?: boolean }): Promise<MutationResult | null>;
  run<T>(fn: () => Promise<T>): Promise<T | undefined>;
  notify(text: string, action?: ToastAction): void;
  open(id: number): void;
  back(): void;
  reload(): void;
  closeIssue(issue: Issue): Promise<void>;
  reopenIssue(issue: Issue): Promise<void>;
}

export const Ctx = createContext<AppContext | null>(null);
export function useApp(): AppContext {
  const value = useContext(Ctx);
  if (!value) throw new Error("AppContext missing");
  return value;
}
