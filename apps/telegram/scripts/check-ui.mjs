#!/usr/bin/env node
// Gate for the committed UI artifacts: (1) rebuilt bytes equal dist/*; (2) no extra files and size budgets;
// (3) no absolute paths or source maps leaked into the output; (4) static scan of ui/src for HTML/code sinks.
// Exit status is non-zero on any failure.
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { buildUi, root } from "./ui-build.mjs";

const MAX_TOTAL = 400_000, MAX_JS = 340_000, MAX_CSS = 40_000;
const problems = [];
const built = await buildUi();
let total = 0;
for (const [name, bytes] of built) {
  total += bytes.length;
  let committed;
  try { committed = readFileSync(join(root, "dist", name)); } catch { problems.push(`dist/${name} is missing; run npm run build:ui`); continue; }
  if (!committed.equals(bytes)) problems.push(`dist/${name} differs from a fresh build; run npm run build:ui and commit the result`);
  const text = bytes.toString("utf8");
  for (const [pattern, why] of [[root.split("\\").join("/"), "absolute repository path"], [/\/Users\/|\/home\/[a-z]|[A-Z]:\\\\Users/, "home directory path"], [/sourceMappingURL/, "source map reference"], [/node_modules\//, "node_modules path"]]) {
    if (typeof pattern === "string" ? text.includes(pattern) : pattern.test(text)) problems.push(`${name} contains a ${why}`);
  }
}
const extra = readdirSync(join(root, "dist")).filter((name) => !built.has(name));
if (extra.length) problems.push(`dist/ contains files the build does not produce: ${extra.join(", ")}`);
if (total > MAX_TOTAL) problems.push(`total ${total} bytes exceeds ${MAX_TOTAL}`);
if ((built.get("app.js")?.length ?? 0) > MAX_JS) problems.push(`app.js exceeds ${MAX_JS} bytes`);
if ((built.get("app.css")?.length ?? 0) > MAX_CSS) problems.push(`app.css exceeds ${MAX_CSS} bytes`);

// The UI renders issue text as text. Keep every route from user content to HTML, code or navigation closed.
const SINKS = [
  [/dangerouslySetInnerHTML|\.innerHTML|\.outerHTML|insertAdjacentHTML|document\.write/, "HTML sink; render user content as text"],
  [/\beval\s*\(|new\s+Function\b|set(?:Timeout|Interval)\s*\(\s*["'`]/, "dynamic code"],
  [/\bimport\s*\(/, "dynamic import"],
  [/\bstyle=["'{]\s*["'`]/, "inline style attribute string (blocked by the style-src CSP)"],
  [/https?:\/\/(?!www\.w3\.org)/, "absolute URL; the page must work below any tunnel prefix"],
];
const walk = (dir) => readdirSync(dir).sort().flatMap((name) => {
  const path = join(dir, name);
  return statSync(path).isDirectory() ? walk(path) : /\.(ts|tsx)$/.test(name) ? [path] : [];
});
for (const path of walk(join(root, "ui/src"))) {
  const rel = relative(join(root, "ui/src"), path).split("\\").join("/");
  readFileSync(path, "utf8").split("\n").forEach((line, index) => {
    for (const [pattern, why] of SINKS) if (pattern.test(line)) problems.push(`${rel}:${index + 1} ${why}`);
    if (rel !== "api.ts" && /\b(fetch|XMLHttpRequest|WebSocket|EventSource)\b/.test(line) && !line.trim().startsWith("//")) problems.push(`${rel}:${index + 1} network access belongs in api.ts`);
  });
}
if (problems.length) { for (const problem of problems) console.error(`check-ui: ${problem}`); process.exit(1); }
console.log(`check-ui: ok (${built.size} files, ${total} bytes)`);
