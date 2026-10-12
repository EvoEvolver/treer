import { readFile, stat } from "node:fs/promises";
import { stdin } from "node:process";
import { exportNativeSession, importNativeSession } from "../src/harness/session-files.js";
import { SessionTransferError, type SessionBundle } from "../src/session-transfer.js";

async function main() {
  const [action, ...argv] = process.argv.slice(2);
  const options = new Map<string, string>();
  for (let i = 0; i < argv.length; i += 2) {
    const key = argv[i], value = argv[i + 1];
    if (!["--harness", "--session-id", "--cwd", "--directory", "--input"].includes(key) || !value || value.startsWith("--") || options.has(key)) throw new Error(`Invalid option: ${key}`);
    options.set(key, value);
  }
  if (action === "export") {
    const harness = options.get("--harness"), id = options.get("--session-id"), cwd = options.get("--cwd");
    if ((harness !== "codex" && harness !== "claude") || !id || !cwd) throw new Error("export requires --harness codex|claude --session-id ID --cwd DIR");
    return exportNativeSession(harness, id, cwd);
  }
  if (action === "import") {
    const directory = options.get("--directory");
    if (!directory) throw new Error("import requires --directory NEW_DIR");
    let raw = "";
    const input = options.get("--input");
    if (input) {
      const info = await stat(input);
      if (!info.isFile() || info.size > 96 * 1024 * 1024) throw new SessionTransferError("too_large", "Session input must be a regular file under 96 MiB");
      raw = await readFile(input, "utf8");
    } else {
      stdin.setEncoding("utf8");
      for await (const chunk of stdin) {
        raw += String(chunk);
        if (Buffer.byteLength(raw) > 96 * 1024 * 1024) throw new SessionTransferError("too_large", "Session input exceeds size limit");
      }
    }
    if (Buffer.byteLength(raw) > 96 * 1024 * 1024) throw new SessionTransferError("too_large", "Session input exceeds size limit");
    let bundle: SessionBundle;
    try { bundle = JSON.parse(raw) as SessionBundle; }
    catch { throw new SessionTransferError("invalid_bundle", "Invalid session bundle JSON"); }
    return importNativeSession(bundle, { directory, sessionId: options.get("--session-id") });
  }
  throw new Error("Usage: session.sh export --harness codex|claude --session-id ID --cwd DIR | session.sh import --directory NEW_DIR [--session-id NEW_UUID] [--input FILE]");
}
main().then((result) => console.log(JSON.stringify(result))).catch((error) => {
  console.error(JSON.stringify({ error: { code: error instanceof SessionTransferError ? error.code : "session_transfer_failed", message: error.message } }));
  process.exitCode = 1;
});
