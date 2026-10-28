// Unified diff text to rows. Context lines get the new-file line number; +/- rows carry a sign so colour is never the only cue.
export interface DiffLine { kind: "hunk" | "header" | "add" | "del" | "ctx"; gutter: string; text: string }

/** Unified diff to rows. Context lines get the new-file line number; +/- rows get a sign (never colour alone). */
export function parseDiff(diff: string): DiffLine[] {
  const lines: DiffLine[] = [];
  let oldLine = 0, newLine = 0, seenHunk = false;
  for (const raw of diff.split("\n")) {
    if (raw.startsWith("@@")) {
      seenHunk = true;
      const match = /^@@ -(\d+)(?:,\d+)? \+(\d+)/.exec(raw);
      oldLine = Number(match?.[1] ?? 0); newLine = Number(match?.[2] ?? 0);
      lines.push({ kind: "hunk", gutter: "", text: raw });
    } else if (!seenHunk && (raw.startsWith("+++") || raw.startsWith("---"))) lines.push({ kind: "header", gutter: "", text: raw });
    else if (raw.startsWith("+")) { lines.push({ kind: "add", gutter: "+", text: raw.slice(1) }); newLine++; }
    else if (raw.startsWith("-")) { lines.push({ kind: "del", gutter: "−", text: raw.slice(1) }); oldLine++; }
    else { lines.push({ kind: "ctx", gutter: newLine ? String(newLine) : "", text: raw.startsWith(" ") ? raw.slice(1) : raw }); newLine++; oldLine++; }
  }
  return lines;
}


/** Added and removed line counts of a unified diff (hunk bodies only). */
export function diffCounts(diff: string): { added: number; removed: number } {
  let added = 0, removed = 0;
  for (const line of parseDiff(diff)) { if (line.kind === "add") added++; else if (line.kind === "del") removed++; }
  return { added, removed };
}
