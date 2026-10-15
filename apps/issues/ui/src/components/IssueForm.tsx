import { useEffect, useRef, useState, type ChangeEvent } from "react";
import { api } from "../api";
import { useApp } from "../context";
import { PRIORITIES } from "../lib";
import type { Issue, MutationResult, Priority } from "../types";
import { Field, PickerField, Select } from "./Fields";
import { AssigneePicker } from "./Picker";

const list = (text: string) => text.split(",").map((s) => s.trim()).filter(Boolean);

export function IssueForm({ issue, parent, done, cancel }: {
  issue?: Issue; parent?: number | null; done: (result: MutationResult) => void; cancel: () => void;
}) {
  const { run } = useApp();
  const [state, setState] = useState({
    title: issue?.title ?? "", description: issue?.description ?? "", assignee: issue?.assignee ?? "", priority: (issue?.priority ?? "normal") as Priority,
    labels: issue?.labels.join(", ") ?? "", parent: String(issue?.parent_id ?? parent ?? ""), blocked: issue?.blocked_by.join(", ") ?? "",
  });
  const [saving, setSaving] = useState(false);
  const title = useRef<HTMLInputElement>(null);
  useEffect(() => title.current?.focus(), []);
  const set = (key: "title" | "description" | "labels" | "parent" | "blocked") =>
    (e: ChangeEvent<HTMLInputElement | HTMLTextAreaElement>) => setState((s) => ({ ...s, [key]: e.target.value }));
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setSaving(true);
    const body: Record<string, unknown> = {
      title: state.title, description: state.description, assignee: state.assignee || null, priority: state.priority,
      labels: list(state.labels), parent_id: state.parent ? Number(state.parent) : null, blocked_by: list(state.blocked).map(Number),
    };
    if (issue) body.version = issue.version;
    try { await run(async () => done(await api<MutationResult>(issue ? `v1/issues/${issue.id}` : "v1/issues", issue ? "PATCH" : "POST", body))); }
    finally { setSaving(false); }
  };
  return (
    <>
      <div className="detail-bar"><button type="button" className="detail-back" onClick={cancel}>‹ Back</button><span className="grow"></span></div>
      <h2>{issue ? "Edit issue" : "New issue"}</h2>
      <form className="form" onSubmit={submit}>
        <Field text="Title"><input aria-label="Title" ref={title} required maxLength={300} value={state.title} onChange={set("title")} /></Field>
        <Field text="Description (Markdown source, rendered as text)">
          <textarea aria-label="Description (Markdown source, rendered as text)" value={state.description} onChange={set("description")}></textarea>
        </Field>
        <PickerField text="Assignee"><AssigneePicker value={state.assignee} onChange={(assignee) => setState((s) => ({ ...s, assignee }))} /></PickerField>
        <Select<Priority> text="Priority" values={PRIORITIES} value={state.priority} onChange={(priority) => setState((s) => ({ ...s, priority }))} />
        <Field text="Labels (comma separated)"><input aria-label="Labels (comma separated)" value={state.labels} onChange={set("labels")} /></Field>
        <Field text="Parent issue ID"><input aria-label="Parent issue ID" type="number" min="1" value={state.parent} onChange={set("parent")} /></Field>
        <Field text="Blocked by issue IDs (comma separated)"><input aria-label="Blocked by issue IDs (comma separated)" value={state.blocked} onChange={set("blocked")} /></Field>
        <div className="buttons">
          <button type="submit" className="primary" disabled={saving}>{issue ? "Save issue" : "Create issue"}</button>
          <button type="button" onClick={cancel}>Cancel</button>
        </div>
      </form>
    </>
  );
}
