import { lstatSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = dirname(fileURLToPath(import.meta.url));
const pointer = join(root, "current.json");
if (!lstatSync(pointer).isFile() || lstatSync(pointer).isSymbolicLink()) throw new Error("Invalid launcher pointer");
const current = JSON.parse(readFileSync(pointer, "utf8"));
if (!/^sha256-[a-f0-9]{64}$/.test(current.version || "")) throw new Error("Invalid launcher version");
process.env.AGENT_UI_TREER_BIN = current.treer;
for (const path of [join(root, current.version), join(root, current.version, "scripts"), join(root, current.version, "scripts/treer-agent.mjs")]) {
  if (lstatSync(path).isSymbolicLink()) throw new Error("Launcher paths must not be symbolic links");
}
await import(pathToFileURL(join(root, current.version, "scripts/treer-agent.mjs")).href);
