import { useCallback, useEffect, useState, type FormEvent, type ReactNode } from "react";
import { api, ApiError } from "./api";
import type { AuditEvent, Binding, PairingCode, Status, User } from "./types";

const REFRESH_MS = 5000;
const when = (seconds: number) => new Date(seconds * 1000).toLocaleString();
const BRIDGE_TEXT: Record<string, string> = {
  running: "Running", paused: "Paused", needs_token: "Waiting for a bot token", starting: "Starting", error: "Error", offline: "Offline — the bridge process is not reporting",
};
const BRIDGE_ERRORS: Record<string, string> = {
  different_bot: "The saved token belongs to a different bot than this bridge was set up with.",
  bot_unavailable: "Telegram could not be reached or rejected the token.", telegram_unavailable: "Telegram could not be reached.", telegram_request_rejected: "Telegram rejected the token.",
};

function Card({ title, children, aside }: { title: string; children: ReactNode; aside?: ReactNode }) {
  return <section className="card"><header><h2>{title}</h2>{aside}</header>{children}</section>;
}

function useCountdown(expiresAt: number | null): number {
  const [now, setNow] = useState(() => Date.now() / 1000);
  useEffect(() => {
    if (expiresAt === null) return;
    const timer = setInterval(() => setNow(Date.now() / 1000), 1000);
    return () => clearInterval(timer);
  }, [expiresAt]);
  return expiresAt === null ? 0 : Math.max(0, Math.floor(expiresAt - now));
}

export function App() {
  const [status, setStatus] = useState<Status | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [pairing, setPairing] = useState<PairingCode | null>(null);

  const refresh = useCallback(async () => {
    try { setStatus(await api<Status>("v1/status")); setError(null); }
    catch (e) { setError(e instanceof ApiError ? e.message : "Request failed"); }
  }, []);
  useEffect(() => {
    void refresh();
    const timer = setInterval(() => { if (!document.hidden) void refresh(); }, REFRESH_MS);
    return () => clearInterval(timer);
  }, [refresh]);

  // Runs a write, reports the fixed server message on failure and refreshes the view.
  const act = useCallback(async (label: string, fn: () => Promise<unknown>): Promise<boolean> => {
    setBusy(true); setNotice(null);
    try { await fn(); setNotice(label); setError(null); await refresh(); return true; }
    catch (e) { setError(e instanceof ApiError ? e.message : "Request failed"); return false; }
    finally { setBusy(false); }
  }, [refresh]);

  if (!status) return <main><h1>Telegram bridge</h1><p role="status">{error ?? "Loading…"}</p></main>;
  const bridge = BRIDGE_TEXT[status.bridge.state] ?? status.bridge.state;
  return (
    <main>
      <header className="top">
        <div><h1>Telegram bridge</h1><p className="muted">Signed in as {status.me.name || status.me.id}{status.me.role ? ` · ${status.me.role}` : ""}</p></div>
        <span className={"pill " + (status.paused ? "warn" : status.bridge.state === "running" ? "ok" : "bad")} id="bridge-state">{status.paused ? "Paused" : bridge}</span>
      </header>
      {error && <p className="banner error" role="alert">{error}</p>}
      {notice && <p className="banner ok" role="status">{notice}</p>}
      {status.bridge.error && <p className="banner error">{BRIDGE_ERRORS[status.bridge.error] ?? "The bridge could not start."}</p>}
      <TokenCard status={status} busy={busy} act={act} />
      <PairingCard status={status} busy={busy} act={act} pairing={pairing} setPairing={setPairing} />
      <BindingsCard bindings={status.bindings} busy={busy} act={act} defaultPoc={status.default_poc} hasBot={status.bot.configured || status.bindings.length > 0} />
      <UsersCard users={status.users} busy={busy} act={act} />
      <Card title="Bridge">
        <p className="muted">Pausing stops forwarding in both directions; messages wait on Telegram and in Treer until you resume.</p>
        <button type="button" disabled={busy} onClick={() => act(status.paused ? "Resumed" : "Paused", () => api(status.paused ? "v1/resume" : "v1/pause", "POST", {}))}>{status.paused ? "Resume" : "Pause"}</button>
      </Card>
      <AuditCard events={status.audit} />
    </main>
  );
}

type Act = (label: string, fn: () => Promise<unknown>) => Promise<boolean>;

function TokenCard({ status, busy, act }: { status: Status; busy: boolean; act: Act }) {
  const [token, setToken] = useState("");
  const [replacing, setReplacing] = useState(false);
  const show = !status.bot.configured || replacing;
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const value = token;
    setToken("");                       // write-only: the secret leaves the page state the moment it is submitted
    if (await act("Token saved and verified with Telegram", () => api("v1/token", "PUT", { token: value }))) setReplacing(false);
  };
  return (
    <Card title="Bot token">
      {status.bot.configured && <p>Bot <strong>@{status.bot.username ?? "unknown"}</strong> · token ending <code>…{status.bot.last4 ?? "????"}</code></p>}
      {show ? (
        <form onSubmit={submit}>
          <label>Token from @BotFather
            <input type="password" name="bot-token" autoComplete="new-password" spellCheck={false} autoCapitalize="off" value={token}
              onChange={(e) => setToken(e.target.value)} placeholder="123456:ABC…" required />
          </label>
          <p className="muted">It is checked with Telegram before it is saved, stored in a private file on the bridge machine, and never shown again.</p>
          <div className="row"><button type="submit" className="primary" disabled={busy || !token}>Save token</button>
            {replacing && <button type="button" onClick={() => { setReplacing(false); setToken(""); }}>Cancel</button>}</div>
        </form>
      ) : (
        <div className="row">
          <button type="button" disabled={busy} onClick={() => setReplacing(true)}>Replace token</button>
          <button type="button" className="danger" disabled={busy} onClick={() => { if (confirm("Remove the bot token? The bridge stops until a new token is saved.")) void act("Token removed", () => api("v1/token", "DELETE", {})); }}>Remove token</button>
        </div>
      )}
    </Card>
  );
}

function PairingCard({ status, busy, act, pairing, setPairing }: { status: Status; busy: boolean; act: Act; pairing: PairingCode | null; setPairing: (code: PairingCode | null) => void }) {
  const [poc, setPoc] = useState("");
  const seconds = useCountdown(pairing?.expires_at ?? null);
  const active = status.pairing;
  useEffect(() => { if (pairing && seconds === 0) setPairing(null); }, [pairing, seconds, setPairing]);
  const bot = status.bot.username ? `@${status.bot.username}` : "your bot";
  return (
    <Card title="Pair a Telegram account">
      {!status.bot.configured ? <p className="muted">Save a bot token first.</p> : (<>
        <p className="muted">Generate a one-time code, then send <code>/pair CODE</code> to {bot} in a <strong>private chat</strong>. That Telegram user is admitted and the chat is bound to the point of contact below.</p>
        <label>Point of contact (agent ID or path)
          <input value={poc} onChange={(e) => setPoc(e.target.value)} placeholder={status.default_poc || "ag_…"} spellCheck={false} />
        </label>
        <div className="row">
          <button type="button" className="primary" disabled={busy} onClick={() => act("Pairing code created", async () => setPairing(await api<PairingCode>("v1/pairing", "POST", { poc: poc || undefined })))}>Generate pairing code</button>
          {(active || pairing) && <button type="button" disabled={busy} onClick={() => { setPairing(null); void act("Pairing code cancelled", () => api("v1/pairing", "DELETE", {})); }}>Cancel code</button>}
        </div>
        {pairing && seconds > 0 && (
          <div className="code" aria-live="polite">
            <div id="pairing-code">{pairing.code}</div>
            <p className="muted">Shown once · expires in {Math.floor(seconds / 60)}:{String(seconds % 60).padStart(2, "0")} · bound to {pairing.poc} · single use</p>
          </div>
        )}
        {!pairing && active && <p className="muted">A code is active until {when(active.expires_at)} (created by {active.created_by}). Its value cannot be shown again — generate a new one to replace it.</p>}
      </>)}
    </Card>
  );
}

function BindingsCard({ bindings, busy, act, defaultPoc, hasBot }: { bindings: Binding[]; busy: boolean; act: Act; defaultPoc: string; hasBot: boolean }) {
  const [conversation, setConversation] = useState("");
  const [target, setTarget] = useState("");
  return (
    <Card title="Conversations and points of contact">
      {bindings.length === 0 ? <p className="muted">No conversation is bound yet — pair an account above.</p> : (
        <table><thead><tr><th>Conversation</th><th>Point of contact</th><th>Wake</th><th>State</th><th></th></tr></thead><tbody>
          {bindings.map((b) => <BindingRow key={b.conversation} b={b} busy={busy} act={act} />)}
        </tbody></table>
      )}
      {hasBot && (
        <form className="row wrap" onSubmit={(e) => { e.preventDefault(); void act("Binding saved", () => api("v1/bindings", "PUT", { conversation, to: target || defaultPoc })).then((ok) => { if (ok) { setConversation(""); setTarget(""); } }); }}>
          <input aria-label="Conversation (chat ID or CHAT:THREAD)" value={conversation} onChange={(e) => setConversation(e.target.value)} placeholder="Chat ID, e.g. 123456 or -100123:5" spellCheck={false} required />
          <input aria-label="Point of contact for the new conversation" value={target} onChange={(e) => setTarget(e.target.value)} placeholder={defaultPoc || "ag_…"} spellCheck={false} />
          <button type="submit" disabled={busy}>Bind</button>
        </form>
      )}
    </Card>
  );
}

function BindingRow({ b, busy, act }: { b: Binding; busy: boolean; act: Act }) {
  const [target, setTarget] = useState(b.target_agent_id);
  useEffect(() => setTarget(b.target_agent_id), [b.target_agent_id]);
  return (
    <tr>
      <td><code>{b.conversation}</code></td>
      <td><input aria-label={`Point of contact for ${b.conversation}`} value={target} onChange={(e) => setTarget(e.target.value)} spellCheck={false} /></td>
      <td><input type="checkbox" aria-label={`Wake the point of contact for ${b.conversation}`} checked={b.wake_agent} disabled={busy}
        onChange={(e) => void act("Binding saved", () => api("v1/bindings", "PUT", { conversation: b.conversation, to: b.target_agent_id, wake_agent: e.target.checked }))} /></td>
      <td>{b.enabled ? "on" : "off"}</td>
      <td className="actions">
        <button type="button" disabled={busy || target === b.target_agent_id} onClick={() => act("Binding saved", () => api("v1/bindings", "PUT", { conversation: b.conversation, to: target }))}>Save</button>
        <button type="button" disabled={busy} onClick={() => act(b.enabled ? "Binding paused" : "Binding resumed", () => api(`v1/bindings/${b.enabled ? "disable" : "enable"}`, "POST", { conversation: b.conversation }))}>{b.enabled ? "Pause" : "Resume"}</button>
      </td>
    </tr>
  );
}

function UsersCard({ users, busy, act }: { users: User[]; busy: boolean; act: Act }) {
  const [id, setId] = useState("");
  const [label, setLabel] = useState("");
  return (
    <Card title="Allowed Telegram users">
      {users.length === 0 ? <p className="muted">Nobody is admitted yet.</p> : (
        <table><thead><tr><th>Telegram ID</th><th>Label</th><th>State</th><th></th></tr></thead><tbody>
          {users.map((u) => <tr key={u.user_id}><td><code>{u.user_id}</code></td><td>{u.label}</td><td>{u.enabled ? "allowed" : "disabled"}</td>
            <td className="actions"><button type="button" disabled={busy} onClick={() => act(u.enabled ? "User disabled" : "User allowed", () => api("v1/users", "PUT", { user_id: u.user_id, label: u.label, enabled: !u.enabled }))}>{u.enabled ? "Disable" : "Allow"}</button></td></tr>)}
        </tbody></table>
      )}
      <form className="row wrap" onSubmit={(e) => { e.preventDefault(); void act("User saved", () => api("v1/users", "PUT", { user_id: id, label, enabled: true })).then((ok) => { if (ok) { setId(""); setLabel(""); } }); }}>
        <input aria-label="Telegram user ID" value={id} onChange={(e) => setId(e.target.value)} placeholder="Telegram numeric user ID" inputMode="numeric" required />
        <input aria-label="Label for the new user" value={label} onChange={(e) => setLabel(e.target.value)} placeholder="Label" maxLength={64} required />
        <button type="submit" disabled={busy}>Add or update</button>
      </form>
    </Card>
  );
}

function AuditCard({ events }: { events: AuditEvent[] }) {
  return (
    <Card title="Recent changes">
      {events.length === 0 ? <p className="muted">Nothing yet.</p> : (
        <ol className="audit">
          {events.map((e, i) => (
            <li key={i}><span className="muted">{when(e.at)}</span> <strong>{e.op}</strong> by <code>{e.actor}</code> via {e.via}
              {e.conversation ? <> · <code>{e.conversation}</code></> : null}{e.old_target || e.new_target ? <> · {e.old_target ?? "—"} → {e.new_target ?? "off"}</> : null}
              {e.outcome && e.outcome !== "ok" ? <> · {e.outcome}</> : null}</li>
          ))}
        </ol>
      )}
    </Card>
  );
}
