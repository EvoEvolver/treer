import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { checkPluginBoundaries } from "./check-plugins.mjs";

function fixture(files) {
  const root = mkdtempSync(join(tmpdir(), "treer-plugin-boundary-test-"));
  const plugin = join(root, "plugins", "fixture");
  mkdirSync(plugin, { recursive: true });
  writeFileSync(join(plugin, "plugin.json"), "{}\n");
  for (const [name, content] of Object.entries(files)) {
    const path = join(plugin, name);
    mkdirSync(join(path, ".."), { recursive: true });
    writeFileSync(path, content);
  }
  return root;
}

test("CLI-only scripts and plugin-owned data sources pass", () => {
  const root = fixture({
    "plugin.py": `
import os
import sqlite3
import subprocess

cli = os.environ["TREER_CLI"]
subprocess.run([cli, "message", "receive"], check=True)
sqlite3.connect("plugin-state.sqlite3")
legacy_postgresql_source = "postgresql://legacy.example/mail"
`,
  });
  try {
    assert.deepEqual(checkPluginBoundaries(root), []);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

const rejected = [
  ["Cargo.toml", "[package]\nname = \"coupled\"\n", "rust-source"],
  ["src/lib.rs", "pub fn coupled() {}\n", "rust-source"],
  ["plugin.py", "token = os.environ['TREER_WORKLOAD_CREDENTIAL']\n", "raw-treer-identity"],
  ["plugin.py", "url = '/agent/workspaces/default/messages'\n", "private-control-route"],
  ["plugin.py", "database = os.environ['DATABASE_URL']\n", "core-database"],
  ["plugin.py", "query = 'SELECT * FROM core_message_outbox'\n", "core-database"],
  ["plugin.py", "subject = 'treer.v1.events.message.created'\n", "core-nats"],
  ["plugin.py", "from treer_protocol import CoreMessage\n", "repository-internal"],
  ["plugin.py", "source = '../../crates/treer-proxy/src/api.rs'\n", "repository-internal"],
];

for (const [file, content, rule] of rejected) {
  test(`rejects ${rule} coupling in ${file}`, () => {
    const root = fixture({ [file]: content });
    try {
      const errors = checkPluginBoundaries(root);
      assert.equal(errors.length, 1, errors.join("\n"));
      assert.match(errors[0], new RegExp(`\\[${rule}\\]`));
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
}

test("rejects package symlinks", () => {
  const root = fixture({ "plugin.py": "print('ok')\n" });
  try {
    const source = join(root, "plugins", "fixture", "plugin.py");
    const linked = join(root, "plugins", "fixture", "linked.py");
    symlinkSync(source, linked);
    assert.match(checkPluginBoundaries(root).join("\n"), /\[symlink\]/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
