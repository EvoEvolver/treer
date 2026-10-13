import type { EventEmitter } from "node:events";

import type { BackgroundTask, Goal, GoalUpdate, ModelOption, ReasoningEffort, TurnDto } from "./model.js";
import type { ImportedSession, SessionBundle, SessionImportOptions } from "./session-transfer.js";

// The adapter boundary. The server, AIS endpoints, file browser, persistence and
// frontend only talk to a HarnessRuntime; each harness (native Codex app-server,
// any ACP agent, ...) implements it. Events: "state" whenever the snapshot
// changes, "log" for diagnostics, and "exit" when the harness process ends.

export interface HarnessInfo {
  /** Stable adapter id, e.g. "codex" or "acp". */
  id: string;
  /** Name shown to people, e.g. "Codex" or "Claude". */
  name: string;
  capabilities: {
    steer: boolean;
    backgroundTasks: boolean;
    goal: { pause: boolean; budget: boolean } | null;
    /** Omitted means unsupported. Advertise only after implementation. */
    sessionTransfer?: { export: boolean; import: boolean };
  };
}

export interface ThreadState {
  id: string;
  title: string;
  cwd: string;
  model: string | null;
  reasoningEffort: ReasoningEffort | null;
  status: "idle" | "running" | "error";
  activeTurnId: string | null;
  lastError: string | null;
  createdAt: string;
  updatedAt: string;
  turns: TurnDto[];
  hasOlderItems: boolean;
}

export interface RuntimeSnapshot {
  ready: boolean;
  thread: ThreadState | null;
  models: ModelOption[];
  backgroundTasks: BackgroundTask[];
  goal: Goal | null;
}

export interface HarnessRuntime extends EventEmitter {
  reconcileGoal?(): Promise<void>;
  readonly harness: HarnessInfo;
  /** Trusted roots for registered background-task output files; never supplied by HTTP clients. */
  readonly taskOutputRoots?: readonly string[];
  /** Starts the harness, resuming the saved or requested session when there is one. */
  start(): Promise<void>;
  stop(): Promise<void>;
  /** Sends a prompt; while a turn runs, the harness steers or queues it. */
  prompt(text: string): Promise<void>;
  interrupt(): Promise<void>;
  updateSettings(input: { model?: string; reasoningEffort?: string | null }): Promise<void>;
  updateGoal?(input: GoalUpdate): Promise<void>;
  clearGoal?(): Promise<void>;
  /** Snapshot the current confirmed native session; never export auth/config files. */
  exportSession?(): Promise<SessionBundle>;
  /** Restore separately; does not change/restart the currently running session. */
  importSession?(bundle: SessionBundle, options: SessionImportOptions): Promise<ImportedSession>;
  snapshot(historyLimit?: number): RuntimeSnapshot;
}

/** Returns the thread with only its most recent `historyLimit` items. */
export function withHistoryLimit(thread: ThreadState, turns: TurnDto[], historyLimit: number): ThreadState {
  const totalItems = turns.reduce((count, turn) => count + turn.items.length, 0);
  let remaining = Math.min(Math.max(1, historyLimit), totalItems);
  const visibleTurns: TurnDto[] = [];
  for (let index = turns.length - 1; index >= 0 && remaining > 0; index -= 1) {
    const turn = turns[index];
    const items = turn.items.slice(-remaining);
    remaining -= items.length;
    visibleTurns.unshift({ ...turn, items });
  }
  return { ...thread, turns: visibleTurns, hasOlderItems: totalItems > historyLimit };
}
