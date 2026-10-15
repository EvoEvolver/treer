// Read-only diagnostic: compare the UI build output with the committed public/ files.
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { buildUi } from "./ui-build.mjs";
const source = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const count = (buffer, byte) => buffer.reduce((n, b) => n + (b === byte ? 1 : 0), 0);
for (const [name, bytes] of await buildUi(source)) {
  const committed = readFileSync(join(source, "public", name));
  let at = -1;
  const length = Math.min(bytes.length, committed.length);
  for (let i = 0; i < length; i++) if (bytes[i] !== committed[i]) { at = i; break; }
  if (at < 0 && bytes.length !== committed.length) at = length;
  const around = (buffer) => at < 0 ? "" : JSON.stringify(buffer.subarray(Math.max(0, at - 30), at + 50).toString("latin1"));
  console.log(`${name} equal=${bytes.equals(committed)} built=${bytes.length}B committed=${committed.length}B built-CR=${count(bytes, 13)} committed-CR=${count(committed, 13)} first-diff=${at}`);
  if (at >= 0) { console.log(`  built:     ${around(bytes)}`); console.log(`  committed: ${around(committed)}`); }
}
