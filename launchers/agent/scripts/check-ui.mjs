#!/usr/bin/env node
// Gate for the committed UI artifacts: (1) rebuilt bytes equal public/{ui.html,boot.js,ui.js,ui.css};
// (2) file count and size budgets; (3) no absolute paths, hostnames or source maps leaked into the output;
// (4) static security scan of ui/src. Exit status is non-zero on any failure.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { buildUi, root } from "./ui-build.mjs";
import { scanSource } from "./check-ui-source.mjs";

const MAX_FILES = 5, MAX_TOTAL = 600_000, MAX_JS = 520_000, MAX_CSS = 80_000;
const problems = [];
const built = await buildUi();
let total = 0;
for (const [name, bytes] of built) {
  total += bytes.length;
  let committed;
  try { committed = readFileSync(join(root, "public", name)); } catch { problems.push(`public/${name} is missing; run npm run build:ui`); continue; }
  if (!committed.equals(bytes)) problems.push(`public/${name} differs from a fresh build; run npm run build:ui and commit the result`);
  const text = bytes.toString("utf8");
  for (const [pattern, why] of [[root.split("\\").join("/"), "absolute repository path"], [/\/Users\/|\/home\/[a-z]|[A-Z]:\\\\Users/, "home directory path"], [/sourceMappingURL/, "source map reference"], [/node_modules\//, "node_modules path"]]) {
    if (typeof pattern === "string" ? text.includes(pattern) : pattern.test(text)) problems.push(`${name} contains a ${why}`);
  }
}
if (built.size > MAX_FILES) problems.push(`${built.size} files exceed the ${MAX_FILES} file budget`);
if (total > MAX_TOTAL) problems.push(`total ${total} bytes exceeds ${MAX_TOTAL}`);
if ((built.get("ui.js")?.length ?? 0) > MAX_JS) problems.push(`ui.js exceeds ${MAX_JS} bytes`);
if ((built.get("ui.css")?.length ?? 0) > MAX_CSS) problems.push(`ui.css exceeds ${MAX_CSS} bytes`);
problems.push(...scanSource(join(root, "ui/src")));
if (problems.length) { for (const problem of problems) console.error(`check-ui: ${problem}`); process.exit(1); }
console.log(`check-ui: ok (${built.size} files, ${total} bytes)`);
