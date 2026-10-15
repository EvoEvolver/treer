export interface Binding { conversation: string; target_agent_id: string; enabled: boolean; wake_agent: boolean; updated_by: string; updated_at: number }
export interface User { user_id: string; label: string; enabled: boolean }
export interface AuditEvent { at: number; actor: string; via: string; op: string; conversation: string | null; old_target: string | null; new_target: string | null; outcome: string }
export interface Status {
  me: { id: string; name: string; kind: string; role: string };
  bot: { configured: boolean; username: string | null; last4: string | null };
  bridge: { state: string; error: string | null; heartbeat_age: number | null };
  paused: boolean;
  pairing: { expires_at: number; created_by: string; poc: string } | null;
  bindings: Binding[];
  users: User[];
  audit: AuditEvent[];
  default_poc: string;
  pair_ttl: number;
}
export interface PairingCode { code: string; expires_at: number; poc: string }
