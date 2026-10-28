import { createContext, useContext, useEffect, useMemo, useReducer, useRef, type Dispatch, type ReactNode } from "react";
import { openSocket, request } from "../api";
import { t } from "../i18n";
import type { GoalInput, PromptMode, Snapshot, ThreadState, TurnDto } from "../types";

// Server truth (snapshot, connection) and UI-local transient state share one reducer. A snapshot always replaces the
// previous one wholesale; there is no client-side merging. Messages are plain strings so no HTML ever enters state.
export interface State {
  snapshot: Snapshot | null;
  connected: boolean;
  error: string | null;
  notice: string | null;
  submitting: boolean;
  loadingOlder: boolean;
  settingsBusy: boolean;
  goalBusy: boolean;
  /** Goal completions shown in the thread, after the turn that was last when the goal finished. */
  notices: Array<{ objective: string; afterTurn: string | undefined }>;
  /** The last conversation seen before the runtime published no thread (provider restart). Shown, marked stale, until a thread returns. */
  staleThread?: ThreadState | null;
}
export type Action =
  | { type: "snapshot"; snapshot: Snapshot; source: "http" | "ws" }
  | { type: "connected"; value: boolean }
  | { type: "error"; message: string | null }
  | { type: "notice"; message: string | null }
  | { type: "submitting"; value: boolean }
  | { type: "loadingOlder"; value: boolean }
  | { type: "settingsBusy"; value: boolean }
  | { type: "goalBusy"; value: boolean };

const initial: State = { snapshot: null, connected: false, error: null, notice: null, submitting: false, loadingOlder: false, settingsBusy: false, goalBusy: false, notices: [] };

// Every snapshot is a fresh object graph. Turns whose content did not change keep their previous identity so memoized
// components skip them while a long conversation streams.
const serialized = new WeakMap<TurnDto, string>();
function json(turn: TurnDto): string {
  let text = serialized.get(turn);
  if (text === undefined) { text = JSON.stringify(turn); serialized.set(turn, text); }
  return text;
}
export function shareTurns(previous: Snapshot | null, next: Snapshot): Snapshot {
  const old = previous?.thread?.turns;
  if (!old || !next.thread) return next;
  const byId = new Map(old.map((turn) => [turn.id, turn]));
  let changed = false;
  const turns = next.thread.turns.map((turn) => {
    const before = byId.get(turn.id);
    if (before && json(before) === JSON.stringify(turn)) { changed = true; return before; }
    return turn;
  });
  return changed ? { ...next, thread: { ...next.thread, turns } } : next;
}

export function reducer(state: State, action: Action): State {
  switch (action.type) {
    case "snapshot": {
      // A late HTTP response must not shrink a history window the user already widened.
      const current = state.snapshot?.history.limit ?? 0;
      if (action.source === "http" && !state.snapshot?.history.page && !action.snapshot.history.page && action.snapshot.history.limit < current) return state;
      // A streamed older page is a bounded view, not a growing cache. Keep its
      // content while live status/queue/goal updates continue over the socket.
      if (action.source !== "http" && state.snapshot?.history.page && action.snapshot.thread && state.snapshot.thread) {
        action = { ...action, snapshot: { ...action.snapshot, history: state.snapshot.history, thread: { ...action.snapshot.thread, turns: state.snapshot.thread.turns, hasOlderItems: state.snapshot.thread.hasOlderItems } } };
      }
      const goal = action.snapshot.goal, before = state.snapshot?.goal;
      const finished = (status?: string) => status === "complete" || status === "done";
      const notices = goal && finished(goal.status) && (!finished(before?.status) || before?.objective !== goal.objective)
        ? [...state.notices, { objective: goal.objective, afterTurn: action.snapshot.thread?.turns.at(-1)?.id }].slice(-20) : state.notices;
      // While the provider restarts, the runtime may publish no thread or an empty one; keep the last one with content.
      const incoming = action.snapshot.thread;
      const hasContent = Boolean(incoming && (incoming.turns.length > 0 || action.snapshot.ready));
      const previous = state.snapshot?.thread?.turns.length ? state.snapshot.thread : state.staleThread ?? null;
      const staleThread = hasContent ? null : previous;
      return { ...state, snapshot: shareTurns(state.snapshot, action.snapshot), error: null, notices, staleThread };
    }
    case "connected": return { ...state, connected: action.value };
    case "error": return { ...state, error: action.message };
    case "notice": return { ...state, notice: action.message };
    case "submitting": return { ...state, submitting: action.value };
    case "loadingOlder": return { ...state, loadingOlder: action.value };
    case "settingsBusy": return { ...state, settingsBusy: action.value };
    case "goalBusy": return { ...state, goalBusy: action.value };
  }
}

interface Store { state: State; dispatch: Dispatch<Action>; actions: Actions }
export interface Actions {
  retry(): void;
  prompt(text: string, mode: PromptMode, attachmentIds?: string[]): Promise<boolean>;
  interrupt(): Promise<void>;
  /** Resolves true when a wider history window arrived. */
  loadOlder(): Promise<boolean>;
  loadLatest(): Promise<void>;
  settings(input: { model?: string; reasoningEffort?: string | null }): Promise<void>;
  cancelQueued(id: string): Promise<void>;
  /** Goal mutations resolve to an error message, or null on success; callers decide where to show it. */
  putGoal(input: GoalInput): Promise<string | null>;
  clearGoal(): Promise<string | null>;
  clearFinishedTasks(): Promise<string | null>;
  reconcileGoal(): Promise<void>;
  retryGoalDelivery(id: string): Promise<void>;
}
const StoreContext = createContext<Store | null>(null);
export function useStore(): Store {
  const store = useContext(StoreContext);
  if (!store) throw new Error("StoreProvider is missing");
  return store;
}

/** Mutations may acknowledge with a full snapshot, a partial body or 204; anything that is not a snapshot is followed by a state read. */
async function asSnapshot(payload: unknown): Promise<Snapshot> {
  return payload && typeof payload === "object" && "harness" in payload ? (payload as Snapshot) : request<Snapshot>("api/state");
}

const historyLimit = (state: State) => state.snapshot?.history.limit ?? 50;

export function StoreProvider({ children }: { children: ReactNode }) {
  const [state, dispatch] = useReducer(reducer, initial);
  const stateRef = useRef(state);
  stateRef.current = state;
  const reconnect = useRef<() => void>(() => undefined);
  const socketRef = useRef<WebSocket | null>(null);
  const failedGoal = useRef<{ value: string; id: string } | null>(null);

  useEffect(() => {
    let socket: WebSocket | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let closed = false;
    let socketSnapshotReceived = false;
    request<Snapshot>("api/state")
      // This read started before the socket. Once a live state arrived, its
      // delayed response (or error) must not rewind the initial history window.
      .then((snapshot) => { if (!closed && !socketSnapshotReceived) dispatch({ type: "snapshot", snapshot, source: "http" }); })
      .catch((error: Error) => { if (!closed && !socketSnapshotReceived) dispatch({ type: "error", message: error.message }); });
    function connect() {
      clearTimeout(timer);
      socket = openSocket();
      socketRef.current = socket;
      socket.addEventListener("open", () => {
        dispatch({ type: "connected", value: true });
        // Re-send the widened history window after every reconnect.
        const limit = historyLimit(stateRef.current);
        if (limit > 50) socket?.send(JSON.stringify({ type: "history-limit", limit }));
      });
      socket.addEventListener("message", (event) => {
        try {
          const payload = JSON.parse(String(event.data)) as Snapshot;
          if (payload?.type === "state") {
            socketSnapshotReceived = true;
            dispatch({ type: "snapshot", snapshot: payload, source: "ws" });
          }
          dispatch({ type: "connected", value: true });
        } catch { /* ignore malformed frames */ }
      });
      socket.addEventListener("close", () => {
        dispatch({ type: "connected", value: false });
        if (!closed) timer = setTimeout(connect, 1000);
      });
    }
    reconnect.current = () => { socket?.close(); connect(); };
    connect();
    return () => { closed = true; clearTimeout(timer); socket?.close(); };
  }, []);

  const actions = useMemo<Actions>(() => ({
    retry: () => reconnect.current(),
    async prompt(text, mode, attachmentIds = []) {
      const snapshot = stateRef.current.snapshot;
      if (!snapshot?.ready || stateRef.current.submitting) return false;
      dispatch({ type: "submitting", value: true });
      dispatch({ type: "error", message: null });
      dispatch({ type: "notice", message: null });
      try {
        const running = snapshot.thread?.status === "running";
        const downgrade = mode === "steer" && running && !snapshot.harness.capabilities.steer;
        const next = await request<Snapshot>("api/prompt", { method: "POST", body: JSON.stringify({ prompt: text, attachment_ids: attachmentIds, mode: downgrade ? "queue" : mode, history_limit: historyLimit(stateRef.current) }) });
        dispatch({ type: "snapshot", snapshot: next, source: "http" });
        if (downgrade || next.submission?.downgraded) dispatch({ type: "notice", message: t("composer.downgraded") });
        return true;
      } catch (error) {
        dispatch({ type: "error", message: (error as Error).message });
        return false;
      } finally {
        dispatch({ type: "submitting", value: false });
      }
    },
    async loadOlder() {
      const current = stateRef.current;
      if (current.loadingOlder || !current.snapshot?.thread?.hasOlderItems) return false;
      dispatch({ type: "loadingOlder", value: true });
      dispatch({ type: "error", message: null });
      try {
        const next = await request<Snapshot>("api/history/older", { method: "POST", body: JSON.stringify({ limit: historyLimit(current), ...(current.snapshot?.history.cursor ? { cursor: current.snapshot.history.cursor } : {}) }) });
        dispatch({ type: "snapshot", snapshot: next, source: "http" });
        const socket = socketRef.current;
        if (!next.history.page && socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify({ type: "history-limit", limit: next.history.limit, respond: false }));
        return true;
      } catch (error) { dispatch({ type: "error", message: (error as Error).message }); return false; }
      finally { dispatch({ type: "loadingOlder", value: false }); }
    },
    async loadLatest() {
      try { dispatch({ type: "snapshot", snapshot: await request<Snapshot>("api/state"), source: "http" }); }
      catch (error) { dispatch({ type: "error", message: (error as Error).message }); }
    },
    async settings(input) {
      dispatch({ type: "settingsBusy", value: true });
      dispatch({ type: "error", message: null });
      try {
        const next = await request<Snapshot>("api/settings", { method: "POST", body: JSON.stringify({ ...input, history_limit: historyLimit(stateRef.current) }) });
        dispatch({ type: "snapshot", snapshot: next, source: "http" });
      } catch (error) { dispatch({ type: "error", message: (error as Error).message }); }
      finally { dispatch({ type: "settingsBusy", value: false }); }
    },
    async cancelQueued(id) {
      try {
        const payload = await request<unknown>(`api/queue/${encodeURIComponent(id)}`, { method: "DELETE" });
        dispatch({ type: "snapshot", snapshot: await asSnapshot(payload), source: "http" });
        dispatch({ type: "error", message: null });
      } catch (error) { dispatch({ type: "error", message: (error as Error).message }); }
    },
    async putGoal(input) {
      if (stateRef.current.goalBusy) return null;
      dispatch({ type: "goalBusy", value: true });
      // A retry of the identical failed request reuses its operation id, so the server can deduplicate it.
      const value = JSON.stringify(input);
      const operationId = failedGoal.current?.value === value ? failedGoal.current.id : crypto.randomUUID();
      failedGoal.current = { value, id: operationId };
      try {
        const payload = await request<Snapshot>("api/goal", { method: "PUT", body: JSON.stringify({ ...input, operationId }) });
        dispatch({ type: "snapshot", snapshot: await asSnapshot(payload), source: "http" });
        failedGoal.current = null;
        return null;
      } catch (error) { return (error as Error).message; }
      finally { dispatch({ type: "goalBusy", value: false }); }
    },
    async clearGoal() {
      if (stateRef.current.goalBusy) return null;
      dispatch({ type: "goalBusy", value: true });
      try {
        const payload = await request<Snapshot>("api/goal", { method: "DELETE" });
        dispatch({ type: "snapshot", snapshot: await asSnapshot(payload), source: "http" });
        return null;
      } catch (error) { return (error as Error).message; }
      finally { dispatch({ type: "goalBusy", value: false }); }
    },
    async clearFinishedTasks() {
      try {
        const payload = await request<Snapshot>("api/tasks/clear-finished", { method: "POST", body: "{}" });
        dispatch({ type: "snapshot", snapshot: await asSnapshot(payload), source: "http" });
        return null;
      } catch (error) { return (error as Error).message; }
    },
    async reconcileGoal() {
      try {
        const payload = await request<Snapshot>("api/goal/reconcile", { method: "POST" });
        dispatch({ type: "snapshot", snapshot: await asSnapshot(payload), source: "http" });
      } catch (error) { dispatch({ type: "error", message: (error as Error).message }); }
    },
    async retryGoalDelivery(id) {
      try {
        const payload = await request<Snapshot>("api/goal/retry", { method: "POST", body: JSON.stringify({ id }) });
        dispatch({ type: "snapshot", snapshot: await asSnapshot(payload), source: "http" });
      } catch (error) { dispatch({ type: "error", message: (error as Error).message }); }
    },
    async interrupt() {
      try {
        const next = await request<Snapshot>("api/interrupt", { method: "POST", body: JSON.stringify({ history_limit: historyLimit(stateRef.current) }) });
        dispatch({ type: "snapshot", snapshot: next, source: "http" });
      } catch (error) { dispatch({ type: "error", message: (error as Error).message }); }
    },
  }), []);

  const value = useMemo(() => ({ state, dispatch, actions }), [state, actions]);
  return <StoreContext.Provider value={value}>{children}</StoreContext.Provider>;
}
