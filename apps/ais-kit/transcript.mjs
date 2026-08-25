export function isUserPromptEntry(entry) {
  const kind = entry?.type ?? entry?.kind;
  if (kind !== "message") return false;
  return (entry.message?.role ?? entry.role) === "user";
}

export function groupTranscriptTurns(entries) {
  const turns = [];
  let current = [];
  let seenUser = false;
  for (const entry of entries ?? []) {
    if (isUserPromptEntry(entry) && seenUser) {
      turns.push(current);
      current = [entry];
    } else {
      current.push(entry);
      if (isUserPromptEntry(entry)) seenUser = true;
    }
  }
  if (current.length) turns.push(current);
  return turns;
}

export function envelopeTranscriptEntry(entry, sessionId, index) {
  return {
    id: String(entry.id ?? entry.entryId ?? `${sessionId}:${index}`),
    kind: String(entry.type ?? entry.kind ?? "unknown"),
    role: typeof entry.message?.role === "string"
      ? entry.message.role
      : typeof entry.role === "string" ? entry.role : null,
    content: entry.message?.content ?? entry.content ?? entry,
    created_at: typeof entry.timestamp === "string"
      ? entry.timestamp
      : typeof entry.created_at === "string" ? entry.created_at : null,
  };
}

export function parseTranscriptPageQuery(searchParams) {
  const pageRaw = searchParams.get("page") ?? searchParams.get("cursor") ?? "0";
  const limitRaw = searchParams.get("limit") ?? "1";
  return {
    page: Math.max(0, Number.parseInt(pageRaw, 10) || 0),
    limit: Math.min(1000, Math.max(1, Number.parseInt(limitRaw, 10) || 1)),
  };
}

export function pageTurns(turns, page, limit) {
  const start = Math.max(0, Math.floor(Number(page)) || 0);
  const count = Math.min(1000, Math.max(1, Math.floor(Number(limit)) || 1));
  const selected = turns.slice(start, start + count);
  const nextPage = start + selected.length < turns.length ? start + selected.length : null;
  return {
    page: start,
    page_count: turns.length,
    next_page: nextPage,
    cursor: String(start),
    next_cursor: nextPage == null ? null : String(nextPage),
    entries: selected.flat(),
  };
}

export function transcriptPageFromEntries(entries, page, limit) {
  return pageTurns(groupTranscriptTurns(entries ?? []), page, limit);
}
