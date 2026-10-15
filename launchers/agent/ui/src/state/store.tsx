import { createContext, useContext, useEffect, useMemo, useReducer, useRef, type Dispatch, type ReactNode } from "react";
import { request, socketUrl } from "../api";
import { t } from "../i18n";
import type { PromptMode, Snapshot, TurnDto } from "../types";

// Server truth (snapshot, connection) and UI-local transient state share one reducer. A snapshot always replaces the
// previous one wholesale; there is no client-side merging. Messages are plain strings so no HTML ever enters state.
export interface State {
  snapshot: Snapshot | null;
  connected: boolean;
  error: string | null;
  notice: string | null;
  submitting: boolean;
  loadingOlder: boolean;
}
export type Action =
  | { type: "snapshot"; snapshot: Snapshot; source: "http" | "ws" }
  | { type: "connected"; value: boolean }
  | { type: "error"; message: string | null }
  | { type: "notice"; message: string | null }
  | { type: "submitting"; value: boolean }
  | { type: "loadingOlder"; value: boolean };

const initial: State = { snapshot: null, connected: false, error: null, notice: null, submitting: false, loadingOlder: false };

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
      if (action.source === "http" && action.snapshot.history.limit < current) return state;
      return { ...state, snapshot: shareTurns(state.snapshot, action.snapshot), error: null };
    }
    case "connected": return { ...state, connected: action.value };
    case "error": return { ...state, error: action.message };
    case "notice": return { ...state, notice: action.message };
    case "submitting": return { ...state, submitting: action.value };
    case "loadingOlder": return { ...state, loadingOlder: action.value };
  }
}

interface Store { state: State; dispatch: Dispatch<Action>; actions: Actions }
export interface Actions {
  retry(): void;
  prompt(text: string, mode: PromptMode): Promise<boolean>;
  interrupt(): Promise<void>;
  /** Resolves true when a wider history window arrived. */
  loadOlder(): Promise<boolean>;
}
const StoreContext = createContext<Store | null>(null);
export function useStore(): Store {
  const store = useContext(StoreContext);
  if (!store) throw new Error("StoreProvider is missing");
  return store;
}

const historyLimit = (state: State) => state.snapshot?.history.limit ?? 50;

export function StoreProvider({ children }: { children: ReactNode }) {
  const [state, dispatch] = useReducer(reducer, initial);
  const stateRef = useRef(state);
  stateRef.current = state;
  const reconnect = useRef<() => void>(() => undefined);
  const socketRef = useRef<WebSocket | null>(null);

  useEffect(() => {
    let socket: WebSocket | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let closed = false;
    request<Snapshot>("api/state")
      .then((snapshot) => dispatch({ type: "snapshot", snapshot, source: "http" }))
      .catch((error: Error) => dispatch({ type: "error", message: error.message }));
    function connect() {
      clearTimeout(timer);
      socket = new WebSocket(socketUrl());
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
          if (payload?.type === "state") dispatch({ type: "snapshot", snapshot: payload, source: "ws" });
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
    async prompt(text, mode) {
      const snapshot = stateRef.current.snapshot;
      if (!snapshot?.ready || stateRef.current.submitting) return false;
      dispatch({ type: "submitting", value: true });
      dispatch({ type: "error", message: null });
      dispatch({ type: "notice", message: null });
      try {
        const running = snapshot.thread?.status === "running";
        const downgrade = mode === "steer" && running && !snapshot.harness.capabilities.steer;
        const next = await request<Snapshot>("api/prompt", { method: "POST", body: JSON.stringify({ prompt: text, mode: downgrade ? "queue" : mode, history_limit: historyLimit(stateRef.current) }) });
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
        const next = await request<Snapshot>("api/history/older", { method: "POST", body: JSON.stringify({ limit: historyLimit(current) }) });
        dispatch({ type: "snapshot", snapshot: next, source: "http" });
        const socket = socketRef.current;
        if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify({ type: "history-limit", limit: next.history.limit, respond: false }));
        return true;
      } catch (error) { dispatch({ type: "error", message: (error as Error).message }); return false; }
      finally { dispatch({ type: "loadingOlder", value: false }); }
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
