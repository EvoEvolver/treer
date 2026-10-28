export type Status = "open" | "in_progress" | "blocked" | "done" | "closed" | "note";
export type Priority = "low" | "normal" | "high" | "urgent";

export interface Principal { id: string; kind: "human" | "agent"; name: string; role?: string; path?: string | null }
export interface Hidden { by: string; at: string }
export interface Progress { total: number; completed: number; truncated?: boolean; by_status: Record<string, number> }
export interface Issue {
  id: number; title: string; description: string; status: Status; priority: Priority; labels: string[];
  assignee: string | null; parent_id: number | null; parent_ids?: number[]; blocked_by: number[]; creator: string;
  created_at: string; updated_at: string; version: number; hidden: Hidden | null;
  progress: Progress; unresolved_dependencies: number[];
}
export interface Notification { status: string; code?: string; message?: string; wake?: Record<string, string>; unresolved_mentions?: string[]; mention_candidates?: Record<string, string[]> }
export interface ActivityEvent {
  id: number; actor: string; kind: string; at: string; summary: string;
  changes?: Record<string, unknown>; body?: string; title?: string; body_bytes?: number; notification: Notification; hidden?: Hidden;
}
export interface Backlink { issue: { id: number; title: string; status: Status }; source: string; event_id: number | null; author: string; at: number }
export interface IssueDetailResult {
  issue: Issue; referenced_by?: Backlink[]; children: Issue[]; children_total: number; activity: ActivityEvent[]; next_activity_offset: number | null;
}
export interface MutationResult extends IssueDetailResult { notification?: Notification }
export interface IssuePage { issues: Issue[]; total: number; next_offset: number | null }

export type Route =
  | { kind: "none" }
  | { kind: "issue"; id: number }
  | { kind: "new"; parent: number | null }
  | { kind: "edit"; issue: Issue };
export interface ToastAction { label: string; run: () => void }
export interface Toast { id: number; text: string; action?: ToastAction }
export interface SearchHit {
  issue: { id: number; title: string; status: Status; assignee: string | null };
  match: { kind: string; event_id: number | null; author: string; at: string };
  snippet: string; snippet_untrusted: true;
}
export interface SearchResult { results: SearchHit[]; count: number; limit: number }
