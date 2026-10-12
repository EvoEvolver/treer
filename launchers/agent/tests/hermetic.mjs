// Tests start servers and scripts that read AGENT_UI_*, CODEX_UI_* and TREER_*
// settings. Running them inside a Treer Agent must not leak that Agent's
// identity, credentials, harness or session into the code under test.
export function hermeticEnv(extra = {}) {
  const env = {};
  for (const [key, value] of Object.entries(process.env)) {
    if (!/^(AGENT_UI_|CODEX_UI_|TREER_)/.test(key) && key !== "CODEX_BIN" && key !== "CLAUDE_BIN") env[key] = value;
  }
  return { ...env, ...extra };
}
