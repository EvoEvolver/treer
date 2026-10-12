// Unified diff for harnesses that report a change as old and new file text
// (ACP `diff` content). Line-based LCS with three lines of context; inputs too
// large for the quadratic table degrade to one whole-file replacement hunk.

const CONTEXT = 3;
const MAX_CELLS = 4_000_000;

type Op = { type: " " | "-" | "+"; line: string };

function lines(value: string) {
  if (!value) return [];
  const parts = value.split("\n");
  if (parts.at(-1) === "") parts.pop();
  return parts;
}

function diffOps(before: string[], after: string[]): Op[] {
  if (before.length * after.length > MAX_CELLS) {
    return [...before.map((line) => ({ type: "-" as const, line })), ...after.map((line) => ({ type: "+" as const, line }))];
  }
  const width = after.length + 1;
  const table = new Uint32Array((before.length + 1) * width);
  for (let i = before.length - 1; i >= 0; i -= 1) {
    for (let j = after.length - 1; j >= 0; j -= 1) {
      table[i * width + j] = before[i] === after[j]
        ? table[(i + 1) * width + j + 1] + 1
        : Math.max(table[(i + 1) * width + j], table[i * width + j + 1]);
    }
  }
  const ops: Op[] = [];
  let i = 0;
  let j = 0;
  while (i < before.length && j < after.length) {
    if (before[i] === after[j]) {
      ops.push({ type: " ", line: before[i] });
      i += 1;
      j += 1;
    } else if (table[(i + 1) * width + j] >= table[i * width + j + 1]) {
      ops.push({ type: "-", line: before[i++] });
    } else {
      ops.push({ type: "+", line: after[j++] });
    }
  }
  while (i < before.length) ops.push({ type: "-", line: before[i++] });
  while (j < after.length) ops.push({ type: "+", line: after[j++] });
  return ops;
}

export function unifiedDiff(path: string, oldText: string | null, newText: string) {
  const before = lines(oldText ?? "");
  const after = lines(newText);
  const ops = diffOps(before, after);
  const header = [`--- ${oldText == null ? "/dev/null" : `a/${path}`}`, `+++ b/${path}`];
  const changed = ops.map((op, index) => (op.type === " " ? -1 : index)).filter((index) => index >= 0);
  if (changed.length === 0) return header.join("\n");
  const hunks: string[] = [];
  let cursor = 0;
  while (cursor < changed.length) {
    const start = Math.max(0, changed[cursor] - CONTEXT);
    let end = changed[cursor];
    while (cursor + 1 < changed.length && changed[cursor + 1] - end <= CONTEXT * 2) end = changed[++cursor];
    end = Math.min(ops.length - 1, end + CONTEXT);
    cursor += 1;
    let oldStart = 1;
    let newStart = 1;
    for (let index = 0; index < start; index += 1) {
      if (ops[index].type !== "+") oldStart += 1;
      if (ops[index].type !== "-") newStart += 1;
    }
    const body = ops.slice(start, end + 1);
    const oldLines = body.filter((op) => op.type !== "+").length;
    const newLines = body.filter((op) => op.type !== "-").length;
    hunks.push(`@@ -${oldLines ? oldStart : oldStart - 1},${oldLines} +${newLines ? newStart : newStart - 1},${newLines} @@`);
    for (const op of body) hunks.push(`${op.type}${op.line}`);
  }
  return [...header, ...hunks].join("\n");
}
