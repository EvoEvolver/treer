#!/usr/bin/env node
// Writes the committed UI artifacts into dist/. Run `npm run build:ui` after changing anything under ui/.
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { buildUi, root } from "./ui-build.mjs";

const files = await buildUi();
mkdirSync(join(root, "dist"), { recursive: true });
let total = 0;
for (const [name, bytes] of files) {
  writeFileSync(join(root, "dist", name), bytes);
  total += bytes.length;
  console.log(`${name.padEnd(10)} ${String(bytes.length).padStart(8)} bytes`);
}
console.log(`total      ${String(total).padStart(8)} bytes`);
