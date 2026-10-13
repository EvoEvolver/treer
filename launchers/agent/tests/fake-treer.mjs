// Local fixture registry validates the same exact manifest fields as Controller.
// It never contacts Treer, enrolls a machine, or touches an existing Agent.
import assert from "node:assert/strict";
import { existsSync, readFileSync, writeFileSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { localFetch } from "./hermetic.mjs";
const root = process.env.FIXTURE_ROOT, args = process.argv.slice(2);
const json = name => JSON.parse(readFileSync(join(root, name), "utf8"));
const save = (name, value) => writeFileSync(join(root, name), JSON.stringify(value), { mode: 0o600 });
if (args.join(" ") === "interface contract") console.log(JSON.stringify(json("contract.json")));
else if (args.join(" ") === "status") console.log('{"agents":[]}');
else if (args[0] === "interface" && args[1] === "clear") save("cleared.json", true);
else if (args[0] === "interface" && args[1] === "register") {
  const value = flag => args[args.indexOf(flag) + 1];
  const port = Number(value("--port"));
  const response = await localFetch(`http://127.0.0.1:${port}/v1/manifest`);
  assert.equal(response.status, 200);
  const manifest = await response.json(), contract = json("contract.json");
  const capabilities = args.flatMap((arg, i) => arg === "--capability" ? [args[i + 1]] : []);
  assert.deepEqual(capabilities.slice().sort(), manifest.capabilities.slice().sort());
  assert.deepEqual(capabilities.slice().sort(), contract.capabilities.slice().sort());
  assert.equal(manifest.protocol, contract.protocol);
  assert.equal(manifest.ui_path, value("--ui-path"));
  assert.equal(manifest.instance_id, value("--instance-id"));
  assert.equal(manifest.instance_id, process.env.AGENT_UI_INSTANCE_ID);
  save("registration.json", { port, manifest }); console.log("{}");
} else if (args.slice(0, 3).join(" ") === "agent admin profile") {
  const [action, name] = args.slice(3), file = join(root, name + ".json");
  if (action === "show") {
    if (!existsSync(file)) { console.error("profile_not_found"); process.exit(1); }
    console.log(readFileSync(file, "utf8"));
  } else if (action === "create" || action === "update") {
    const i = args.indexOf("--cwd"), command = action === "create" ? args[i + 2] : args[args.indexOf("--command") + 1];
    const tail = action === "create" ? args.slice(args.indexOf("--") + 1) : args.flatMap((v, j) => v === "--arg" ? [args[j + 1]] : []);
    save(name + ".json", { cwd: args[i + 1], command, args: tail });
  } else if (action === "delete") unlinkSync(file);
  else throw new Error("Unexpected fake profile operation");
} else throw new Error("Unexpected sealed CLI operation");
