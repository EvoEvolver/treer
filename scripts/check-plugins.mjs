import { existsSync, lstatSync, readdirSync, readFileSync } from "node:fs";
import { basename, extname, relative, resolve, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const repositoryRoot = resolve(fileURLToPath(new URL("..", import.meta.url)));
const ignoredDirectories = new Set([
  "__pycache__",
  ".git",
  ".pytest_cache",
  "dist",
  "node_modules",
  "target",
]);
const sourceExtensions = new Set([
  ".cjs",
  ".js",
  ".jsx",
  ".mjs",
  ".py",
  ".sh",
  ".ts",
  ".tsx",
]);
const forbiddenFiles = [
  {
    rule: "rust-source",
    matches: (name) => name === "Cargo.toml" || extname(name) === ".rs",
    message: "first-party plugins must be scripts and may not contain Rust or Cargo packages",
  },
];
const forbiddenSource = [
  {
    rule: "raw-treer-identity",
    expression:
      /\bTREER_(?:AGENT_ID|AGENT_SERVER_URL|ENROLLMENT_KEY|MACHINE_TOKEN|OPERATOR_CREDENTIAL|SERVER_ID|WORKLOAD_CREDENTIAL|WORKSPACE_ID)\b/,
    message: "plugins may not read or inject raw Treer identity, credential, or Controller variables",
  },
  {
    rule: "private-control-route",
    expression: /(?:\/agent\/(?:connect|machine|workspaces)\b|\bagent\/workspaces\/)/,
    message: "plugins may not call private Proxy or Controller routes",
  },
  {
    rule: "core-database",
    expression:
      /\b(?:DATABASE_URL|TREER_DATABASE_URL|core_messages|core_message_deliveries|core_message_contexts|core_message_idempotency|core_message_outbox|core_plugin_human_sessions|core_plugin_oauth_states|workspace_policies)\b/,
    message: "plugins may not connect to or query Treer Core persistence",
  },
  {
    rule: "core-nats",
    expression: /(?:\bTREER_NATS_[A-Z0-9_]+\b|\btreer\.v1\.(?:cluster|events)\b)/,
    message: "plugins may not consume or publish Treer Core NATS subjects",
  },
  {
    rule: "repository-internal",
    expression:
      /(?:\bcrates\/treer-|(?:\.\.\/)+crates\/|\btreer_(?:agent_host|agent_runtime|agent_server|host_protocol|protocol|proxy)\b)/,
    message: "plugins may not import repository paths or internal Treer modules",
  },
];

function filesBelow(directory) {
  const files = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (entry.isDirectory() && ignoredDirectories.has(entry.name)) {
      continue;
    }
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...filesBelow(path));
    } else if (entry.isFile() || entry.isSymbolicLink()) {
      files.push(path);
    }
  }
  return files;
}

function lineNumber(content, offset) {
  return content.slice(0, offset).split("\n").length;
}

function firstPartyPluginDirectories(pluginsRoot) {
  if (!existsSync(pluginsRoot)) {
    return [];
  }
  return readdirSync(pluginsRoot, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => resolve(pluginsRoot, entry.name))
    .filter((directory) => existsSync(resolve(directory, "plugin.json")))
    .sort();
}

export function checkPluginBoundaries(root = repositoryRoot) {
  const pluginsRoot = resolve(root, "plugins");
  const pluginDirectories = firstPartyPluginDirectories(pluginsRoot);
  const errors = [];
  if (pluginDirectories.length === 0) {
    errors.push("plugins: no first-party plugin packages were found");
    return errors;
  }

  for (const pluginDirectory of pluginDirectories) {
    for (const file of filesBelow(pluginDirectory)) {
      const display = relative(root, file).split(sep).join("/");
      const metadata = lstatSync(file);
      if (metadata.isSymbolicLink()) {
        errors.push(`${display} [symlink]: plugin packages may not contain symbolic links`);
        continue;
      }
      const structural = forbiddenFiles.find((rule) => rule.matches(basename(file)));
      if (structural) {
        errors.push(`${display} [${structural.rule}]: ${structural.message}`);
        continue;
      }
      if (!sourceExtensions.has(extname(file))) {
        continue;
      }
      const content = readFileSync(file, "utf8");
      for (const rule of forbiddenSource) {
        const match = rule.expression.exec(content);
        if (match) {
          errors.push(
            `${display}:${lineNumber(content, match.index)} [${rule.rule}]: ${rule.message}`,
          );
        }
      }
    }
  }
  return errors;
}

export function reportPluginBoundaries(root = repositoryRoot) {
  const errors = checkPluginBoundaries(root);
  if (errors.length > 0) {
    console.error("Plugin boundary check failed:\n");
    for (const error of errors) {
      console.error(`- ${error}`);
    }
    return false;
  }
  console.log("Plugin boundary check passed.");
  return true;
}

const invokedPath = process.argv[1] ? pathToFileURL(resolve(process.argv[1])).href : "";
if (invokedPath === import.meta.url && !reportPluginBoundaries()) {
  process.exitCode = 1;
}
