import { execFile } from "node:child_process";
import { hostname } from "node:os";
import { relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

import { lunaFallbackEnv } from "../apps/ais-kit/provider-env.mjs";

const execFileAsync = promisify(execFile);
const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const selected = new Set(
  (process.env.AIS_E2E_PLATFORMS ?? "")
    .split(",")
    .map((value) => value.trim())
    .filter(Boolean),
);

const platforms = [
  {
    id: "pi",
    binary: "pi",
    name: "ais-e2e-pi",
    kind: "shell",
    command: ["env", "PI_UI_PORT=0", "pi", "--extension", "apps/pi-ui/extension.mjs", "--approve"],
  },
  {
    id: "codex",
    binary: "codex",
    name: "ais-e2e-codex",
    command: ["apps/codex-ais/scripts/treer-agent.sh"],
  },
  {
    id: "opencode",
    binary: "opencode",
    name: "ais-e2e-opencode",
    command: ["apps/opencode-ais/scripts/treer-agent.sh"],
  },
  {
    id: "dsh",
    binary: "dsh",
    name: "ais-e2e-dsh",
    command: ["apps/dsh-ais/scripts/treer-agent.sh"],
  },
  {
    id: "claude",
    binary: "claude",
    name: "ais-e2e-claude",
    command: ["apps/claude-ais/scripts/treer-agent.sh"],
  },
  {
    id: "grok",
    binary: "grok",
    name: "ais-e2e-grok",
    command: ["apps/grok-ais/scripts/treer-agent.sh"],
  },
  {
    id: "cursor",
    binary: "cursor-agent",
    name: "ais-e2e-cursor",
    command: ["apps/cursor-ais/scripts/treer-agent.sh"],
  },
];

function uniquePath() {
  const seen = new Set();
  const parts = [];
  for (const part of [
    resolve(root, "target/debug"),
    process.env.HOME ? resolve(process.env.HOME, ".local/bin") : "",
    ...(process.env.PATH ?? "").split(":"),
  ]) {
    if (!part || seen.has(part)) continue;
    seen.add(part);
    parts.push(part);
  }
  return parts.join(":");
}

function redactCli(text) {
  return String(text ?? "").replace(
    /\b([A-Z][A-Z0-9_]*(?:KEY|TOKEN|SECRET|PASSWORD|CREDENTIAL|AUTHORIZATION))\s*=\s*\S+/gi,
    "$1=<redacted>",
  );
}

function which(binary) {
  return execFileAsync("which", [binary]).then(() => true).catch(() => false);
}

function entryText(entry) {
  const content = entry?.content;
  if (typeof content === "string") return content;
  return JSON.stringify(content ?? entry ?? "");
}

async function treerJson(args) {
  try {
    const { stdout } = await execFileAsync("treer", args, {
      encoding: "utf8",
      maxBuffer: 8 * 1024 * 1024,
    });
    return JSON.parse(stdout);
  } catch (error) {
    const detail = error.stdout || error.stderr || error.message;
    throw new Error(`treer ${redactCli(args.join(" "))} failed: ${redactCli(detail)}`);
  }
}

async function waitForInterface(name, timeoutMs = 120000) {
  const started = Date.now();
  let last = null;
  while (Date.now() - started < timeoutMs) {
    last = await treerJson(["agent", "show", name]);
    if (last.status === "exited" || last.status === "failed") {
      throw new Error(`${name} exited before AIS registration: ${JSON.stringify({
        status: last.status,
        exit_code: last.exit_code,
        interface: last.interface,
      })}`);
    }
    const capabilities = last.interface?.capabilities ?? [];
    if (
      last.interface?.protocol === "treer.agent-interface/v1"
      && capabilities.includes("prompt.submit")
      && capabilities.includes("transcript.read")
      && capabilities.includes("state.observe")
    ) {
      return last;
    }
    await new Promise((resolve) => setTimeout(resolve, 1000));
  }
  throw new Error(`${name} did not register AIS capabilities in time: ${JSON.stringify(last?.interface ?? last?.status)}`);
}

function pickMachine(machines) {
  const local = hostname();
  return machines.find((machine) => root.startsWith(machine.root))
    || machines.find((machine) => machine.hostname === local)
    || machines[0];
}

async function runPlatform(platform, machine, envPairs) {
  const cwd = relative(machine.root, root) || ".";
  const sibling = `${platform.name}-b`;
  for (const name of [platform.name, sibling]) {
    await treerJson(["agent", "admin", "delete", name]).catch(() => {});
  }
  const createArgs = [
    "agent", "admin", "create",
    "--machine", machine.server_id,
    "--kind", platform.kind ?? "command",
    "--name", platform.name,
    "--cwd", cwd,
    "--",
    ...(envPairs.length ? ["env", ...envPairs] : []),
    ...platform.command,
  ];
  await treerJson(createArgs);
  try {
    await waitForInterface(platform.name);
    await treerJson([
      "agent", "prompt", platform.name,
      "Reply with exactly PING-1 and nothing else.",
      "--wait", "--timeout", "180000",
    ]);
    const first = await treerJson(["agent", "transcript", platform.name, "--page", "0"]);
    if (first.page_count < 1 || !first.entries?.length) {
      throw new Error(`${platform.id} page 0 was empty: ${JSON.stringify(first)}`);
    }
    await treerJson([
      "agent", "prompt", platform.name,
      "Reply with exactly PING-2 and nothing else.",
      "--wait", "--timeout", "180000",
    ]);
    const page0 = await treerJson(["agent", "transcript", platform.name, "--page", "0"]);
    const page1 = await treerJson(["agent", "transcript", platform.name, "--page", "1"]);
    if (page0.page !== 0 || page1.page !== 1 || page1.page_count < 2) {
      throw new Error(`${platform.id} did not expose two transcript pages: ${JSON.stringify({ page0, page1 })}`);
    }
    await treerJson([
      "agent", "admin", "create",
      "--machine", machine.server_id,
      "--kind", platform.kind ?? "command",
      "--name", sibling,
      "--cwd", cwd,
      "--",
      ...(envPairs.length ? ["env", ...envPairs] : []),
      ...platform.command,
    ]);
    try {
      await waitForInterface(sibling);
      await treerJson([
        "agent", "prompt", sibling,
        "Reply with exactly PING-B and nothing else.",
        "--wait", "--timeout", "180000",
      ]);
      const original = await treerJson(["agent", "transcript", platform.name, "--page", "1"]);
      const other = await treerJson(["agent", "transcript", sibling, "--page", "0"]);
      const originalText = (original.entries ?? []).map(entryText).join("\n");
      const otherText = (other.entries ?? []).map(entryText).join("\n");
      if (otherText.includes("PING-1") || originalText.includes("PING-B")) {
        throw new Error(`${platform.id} leaked transcript across Agents`);
      }
    } finally {
      await treerJson(["agent", "admin", "delete", sibling]).catch(() => {});
    }
  } finally {
    await treerJson(["agent", "admin", "delete", platform.name]).catch(() => {});
  }
}

try {
  await execFileAsync("treer", ["status"], { encoding: "utf8" });
} catch (error) {
  console.error("Live AIS e2e requires a reachable Treer CLI/control plane.");
  console.error(error.stderr || error.message);
  process.exit(1);
}

const machines = await treerJson(["machine", "list"]);
if (!Array.isArray(machines) || machines.length === 0) {
  throw new Error("AIS e2e needs at least one enrolled machine");
}
const machine = pickMachine(machines);
if (!machine?.root || !machine?.server_id) {
  throw new Error(`AIS e2e could not resolve a machine root: ${JSON.stringify(machine)}`);
}

const luna = await lunaFallbackEnv();
const nativeAnthropic = Boolean(process.env.ANTHROPIC_API_KEY || process.env.ANTHROPIC_AUTH_TOKEN);
const env = {
  ...luna,
  PATH: uniquePath(),
  AIS_MODEL: "gpt-5.6-luna",
  CODEX_AIS_MODEL: "gpt-5.6-luna",
  DSH_PROVIDER: process.env.DSH_PROVIDER || "openai",
};
if (!nativeAnthropic) env.CLAUDE_MODEL = "gpt-5.6-luna";
const envPairs = Object.entries(env).map(([key, value]) => `${key}=${value}`);

const reports = [];
for (const platform of platforms) {
  if (selected.size && !selected.has(platform.id)) continue;
  if (!await which(platform.binary)) {
    reports.push(`${platform.id}: skipped (missing ${platform.binary})`);
    continue;
  }
  process.stdout.write(`AIS e2e ${platform.id}...\n`);
  await runPlatform(platform, machine, envPairs);
  reports.push(`${platform.id}: passed`);
}

if (!reports.some((line) => line.endsWith("passed"))) {
  throw new Error(`AIS e2e ran no platforms. ${reports.join("; ") || "none selected"}`);
}
for (const line of reports) console.log(line);
