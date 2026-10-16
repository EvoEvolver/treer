// Tests start servers and scripts that read AGENT_UI_*, CODEX_UI_* and TREER_*
// settings. Running them inside a Treer Agent must not leak that Agent's
// identity, credentials, harness or session into the code under test.
import { request } from "node:http";

// Native local-client fixture: unlike Node fetch, do not synthesize browser
// Sec-Fetch-Mode. Explicit test Origin/Fetch Metadata headers remain untouched.
export function localFetch(input, options = {}) {
  const url = new URL(input);
  if (url.protocol !== "http:" || !["127.0.0.1", "localhost", "[::1]"].includes(url.hostname)) throw new Error("localFetch only accepts sealed loopback fixtures");
  return new Promise((resolve, reject) => {
    const req = request(url, { method: options.method || "GET", headers: options.headers, signal: options.signal }, (response) => {
      const chunks = [];
      response.on("data", (chunk) => chunks.push(chunk));
      response.on("end", () => resolve(new Response(Buffer.concat(chunks), { status: response.statusCode, headers: response.headers })));
      response.on("error", reject);
    });
    req.on("error", reject); req.end(options.body);
  });
}
export function hermeticEnv(extra = {}) {
  const env = {};
  for (const [key, value] of Object.entries(process.env)) {
    if (!/^(AGENT_UI_|CODEX_UI_|TREER_)/.test(key) && key !== "CODEX_BIN" && key !== "CLAUDE_BIN") env[key] = value;
  }
  return { ...env, ...extra };
}
