#!/usr/bin/env node
// Writes the committed UI artifacts into public/. Run `npm run build:ui` after changing anything under ui/.
import { mkdirSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { buildUi, root } from "./ui-build.mjs";

const files = await buildUi();
mkdirSync(join(root, "public"), { recursive: true });
// Remove only obsolete generated fingerprinted assets, never arbitrary public
// files; otherwise bundle preparation could quietly ship dead PDF engines.
for (const name of readdirSync(join(root, "public"))) {
  if (/^(?:[A-Za-z]+-[A-Z0-9]{8}|pdf-worker-[a-f0-9]{16})\.js$/.test(name) && !files.has(name)) rmSync(join(root, "public", name));
}
let total = 0;
for (const [name, bytes] of files) {
  writeFileSync(join(root, "public", name), bytes);
  total += bytes.length;
  console.log(`${name.padEnd(8)} ${String(bytes.length).padStart(8)} bytes`);
}
console.log(`total    ${String(total).padStart(8)} bytes`);
