import { homedir } from "node:os";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

const LUNA_MODEL = "gpt-5.6-luna";

function parseTomlString(source, key) {
  const match = source.match(new RegExp(`^${key}\\s*=\\s*"(.*)"\\s*$`, "m"));
  return match?.[1] ?? null;
}

export async function readCodexCompatibleProvider() {
  const home = join(homedir(), ".codex");
  let baseUrl = process.env.OPENAI_BASE_URL || process.env.ANTHROPIC_BASE_URL || "";
  let apiKey = process.env.OPENAI_API_KEY || process.env.ANTHROPIC_API_KEY
    || process.env.ANTHROPIC_AUTH_TOKEN || "";
  try {
    const config = await readFile(join(home, "config.toml"), "utf8");
    baseUrl ||= parseTomlString(config, "base_url") || "";
  } catch {
    // Missing Codex config is fine; env still applies.
  }
  try {
    const auth = JSON.parse(await readFile(join(home, "auth.json"), "utf8"));
    apiKey ||= auth.OPENAI_API_KEY || auth.tokens?.access_token || auth.api_key || "";
  } catch {
    // Missing Codex auth is fine; env still applies.
  }
  return {
    baseUrl,
    apiKey,
    model: process.env.AIS_MODEL || process.env.MODEL || LUNA_MODEL,
  };
}

export async function lunaFallbackEnv() {
  const provider = await readCodexCompatibleProvider();
  if (!provider.baseUrl || !provider.apiKey) return {};
  const env = {
    OPENAI_BASE_URL: provider.baseUrl,
    OPENAI_API_KEY: provider.apiKey,
    ANTHROPIC_BASE_URL: process.env.ANTHROPIC_BASE_URL || provider.baseUrl,
    ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY || provider.apiKey,
    ANTHROPIC_AUTH_TOKEN: process.env.ANTHROPIC_AUTH_TOKEN || provider.apiKey,
  };
  if (process.env.AIS_MODEL || process.env.MODEL || process.env.CODEX_AIS_MODEL || process.env.CLAUDE_MODEL) {
    env.MODEL = provider.model;
    env.AIS_MODEL = provider.model;
    env.CODEX_AIS_MODEL = process.env.CODEX_AIS_MODEL || provider.model;
    env.CLAUDE_MODEL = process.env.CLAUDE_MODEL || provider.model;
  }
  return env;
}

export async function mergeProviderEnv(base = process.env) {
  const fallback = await lunaFallbackEnv();
  const env = { ...base };
  for (const [key, value] of Object.entries(fallback)) {
    if (!env[key]) env[key] = value;
  }
  return env;
}
