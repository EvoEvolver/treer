// Read registration metadata from the running AIS, never a second capability list.
import { get } from "node:http";
import { execFileSync } from "node:child_process";

const port = Number(process.argv[2]);
if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error("Invalid AIS port");
const manifest = await new Promise((resolve, reject) => {
  const request = get(`http://127.0.0.1:${port}/v1/manifest`, (response) => {
    let body = "";
    response.setEncoding("utf8");
    response.on("data", (chunk) => {
      body += chunk;
      if (body.length > 65536) request.destroy(new Error("AIS manifest too large"));
    });
    response.on("error", reject);
    response.on("end", () => {
      try {
        if (response.statusCode !== 200) throw new Error(`AIS manifest HTTP ${response.statusCode}`);
        resolve(JSON.parse(body));
      } catch (error) { reject(error); }
    });
  });
  request.setTimeout(10000, () => request.destroy(new Error("AIS manifest timed out")));
  request.on("error", reject);
});
// The CLI currently registers v1 only. Fail explicitly if the AIS changes protocol.
if (manifest.protocol !== "treer.agent-interface/v1") throw new Error("Unsupported AIS registration protocol");
if (manifest.instance_id !== process.env.AGENT_UI_INSTANCE_ID || !manifest.instance_id) throw new Error("AIS registration instance mismatch");
if (!Array.isArray(manifest.capabilities) || !manifest.capabilities.every((value) => typeof value === "string" && /^[a-zA-Z0-9._-]+$/.test(value))) throw new Error("Invalid AIS capabilities");
const args = ["interface", "register", "--port", String(port), "--instance-id", manifest.instance_id];
for (const capability of new Set(manifest.capabilities)) args.push("--capability", capability);
if (manifest.ui_path != null) {
  if (typeof manifest.ui_path !== "string" || !manifest.ui_path.startsWith("/")) throw new Error("Invalid AIS UI path");
  args.push("--ui-path", manifest.ui_path);
}
execFileSync("treer", args, { stdio: "inherit" });
