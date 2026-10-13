import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { chmod, lstat, mkdir, mkdtemp, readFile, readdir, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import test from "node:test";
import { exportNativeSession, importNativeSession } from "../src/harness/session-files.js";
import type { SessionBundle, SessionFile } from "../src/session-transfer.js";
import { hermeticEnv } from "./hermetic.mjs";

const launcher = resolve(import.meta.dirname, "..");
const key = (cwd: string) => cwd.replace(/[^a-zA-Z0-9]/g, "-");
const rows = (text: string) => text.trim().split("\n").map((line) => JSON.parse(line));
function file(scope: SessionFile["scope"], path: string, content: string): SessionFile {
  return { scope, path, encoding: "base64", content: Buffer.from(content).toString("base64"), sha256: createHash("sha256").update(content).digest("hex") };
}
async function fixture(harness: "codex" | "claude") {
  const root = await realpath(await mkdtemp(join(tmpdir(), "session-transfer-")));
  const cwd = join(root, "project"), home = join(root, "provider"), id = randomUUID();
  await mkdir(cwd); await mkdir(home);
  const source = harness === "codex" ? join(home, "sessions/2026/10/12", `rollout-2026-10-12T12-00-00-${id}.jsonl`) : join(home, "projects", key(cwd), `${id}.jsonl`);
  await mkdir(dirname(source), { recursive: true });
  const history = harness === "codex" ? [
    { type: "session_meta", payload: { id, cwd, forked_from_id: "old-parent", originator: "codex_cli_rs" } },
    { type: "turn_context", payload: { cwd } },
    { type: "response_item", payload: { type: "message", role: "user", content: [{ type: "input_text", text: "remember this" }] } },
  ] : [
    { type: "user", sessionId: id, cwd, uuid: randomUUID(), message: { role: "user", content: "remember this" } },
    { type: "assistant", sessionId: id, cwd, message: { role: "assistant", content: [{ type: "text", text: "remembered" }] } },
  ];
  await writeFile(source, history.map((row) => JSON.stringify(row)).join("\n") + "\n");
  await writeFile(join(home, "auth.json"), "DO_NOT_EXPORT_AUTH");
  await writeFile(join(home, "config.toml"), "DO_NOT_EXPORT_CONFIG");
  await writeFile(join(cwd, "CLAUDE.md"), "Project instructions");
  await writeFile(join(cwd, "AGENTS.md"), "Agent instructions");
  await writeFile(join(cwd, ".env"), "DO_NOT_EXPORT_ENV");
  await mkdir(join(cwd, ".claude"));
  await writeFile(join(cwd, ".claude/settings.json"), "DO_NOT_EXPORT_SETTINGS");
  if (harness === "claude") {
    await mkdir(join(dirname(source), "memory"));
    await writeFile(join(dirname(source), "memory/MEMORY.md"), "Project memory");
    await writeFile(join(dirname(source), "memory/auth.json"), "DO_NOT_EXPORT_MEMORY_AUTH");
    await mkdir(join(dirname(source), id, "subagents"), { recursive: true });
    await writeFile(join(dirname(source), id, "subagents/agent-a.jsonl"), JSON.stringify({ type: "user", sessionId: id, cwd, message: { role: "user", content: "subtask" } }) + "\n");
  } else {
    await mkdir(join(home, "shell_snapshots"));
    await writeFile(join(home, "shell_snapshots", `${id}.sh`), "DO_NOT_EXPORT_SHELL_CREDENTIALS");
    await mkdir(join(cwd, ".codex/memory"), { recursive: true });
    await writeFile(join(cwd, ".codex/memory/MEMORY.md"), "Codex project memory");
  }
  return { root, cwd, home, id, source };
}

for (const harness of ["codex", "claude"] as const) {
  test(`${harness} session allowlist is not content redaction: synthetic conversation/memory secrets remain`, async (t) => {
    const f = await fixture(harness); t.after(() => rm(f.root, { recursive: true, force: true }));
    const marker = "SYNTHETIC_CONVERSATION_SECRET";
    await writeFile(f.source, (await readFile(f.source, "utf8")).replace("remember this", marker));
    await writeFile(join(f.cwd, "MEMORY.md"), marker);
    const bundle = await exportNativeSession(harness, f.id, f.cwd, f.home);
    assert.ok(bundle.files.some((file) => file.scope === "session" && Buffer.from(file.content, "base64").includes(Buffer.from(marker))));
    assert.ok(bundle.files.some((file) => file.path === "MEMORY.md" && Buffer.from(file.content, "base64").includes(Buffer.from(marker))));
  });
  test(`${harness} exports only explicit session/project memory and imports a fresh native identity without changing the source`, async (t) => {
    const f = await fixture(harness); t.after(() => rm(f.root, { recursive: true, force: true }));
    const original = await readFile(f.source, "utf8");
    const bundle = await exportNativeSession(harness, f.id, f.cwd, f.home);
    assert.equal(bundle.sessionId, f.id);
    assert.ok(bundle.files.some((file) => file.path === "CLAUDE.md"));
    assert.ok(!bundle.files.some((file) => /auth|config|settings|shell_snapshot|\.env/.test(file.path)));
    assert.ok(!JSON.stringify(bundle).includes("DO_NOT_EXPORT"));
    const destination = join(f.root, "restored");
    const imported = await importNativeSession(bundle, { directory: destination }, f.home);
    assert.notEqual(imported.sessionId, f.id);
    assert.equal(imported.cwd, destination);
    assert.equal(await readFile(join(destination, "CLAUDE.md"), "utf8"), "Project instructions");
    let restored: string;
    if (harness === "codex") {
      const home = imported.environment.CODEX_HOME;
      const sessions = await readdir(join(home, "sessions"));
      restored = join(home, "sessions", sessions[0]);
      assert.ok((await lstat(join(home, "auth.json"))).isSymbolicLink());
      assert.equal(await realpath(join(home, "auth.json")), join(f.home, "auth.json"));
      assert.equal(rows(await readFile(restored, "utf8"))[0].payload.id, imported.sessionId);
      assert.equal(rows(await readFile(restored, "utf8"))[0].payload.forked_from_id, undefined);
      assert.equal(await readFile(join(destination, ".codex/memory/MEMORY.md"), "utf8"), "Codex project memory");
    } else {
      assert.equal(imported.environment.CLAUDE_CONFIG_DIR, f.home);
      const project = join(f.home, "projects", key(destination));
      restored = join(project, `${imported.sessionId}.jsonl`);
      assert.equal(rows(await readFile(restored, "utf8"))[0].sessionId, imported.sessionId);
      assert.equal(await readFile(join(project, "memory/MEMORY.md"), "utf8"), "Project memory");
      assert.equal(rows(await readFile(join(project, imported.sessionId, "subagents/agent-a.jsonl"), "utf8"))[0].sessionId, imported.sessionId);
    }
    assert.equal((await lstat(restored)).mode & 0o777, 0o600);
    assert.equal((await lstat(destination)).mode & 0o777, 0o700);
    const history = rows(await readFile(restored, "utf8"));
    assert.equal(harness === "codex" ? history[0].payload.cwd : history[0].cwd, destination);
    assert.equal(await readFile(f.source, "utf8"), original);
    const again = await exportNativeSession(harness, imported.sessionId, destination, harness === "codex" ? imported.environment.CODEX_HOME : f.home);
    assert.equal(again.sessionId, imported.sessionId);
  });
}

test("export rejects incomplete or mismatched native JSONL and external symlinks", async (t) => {
  const f = await fixture("claude"); t.after(() => rm(f.root, { recursive: true, force: true }));
  await writeFile(f.source, '{}');
  await assert.rejects(exportNativeSession("claude", f.id, f.cwd, f.home), /complete JSONL/);
  await writeFile(f.source, JSON.stringify({ type: "user", sessionId: randomUUID() }) + "\n");
  await assert.rejects(exportNativeSession("claude", f.id, f.cwd, f.home), /does not match/);
  await writeFile(f.source, JSON.stringify({ type: "user", sessionId: f.id }) + "\n");
  await rm(join(f.cwd, "CLAUDE.md"));
  await symlink(join(f.home, "auth.json"), join(f.cwd, "CLAUDE.md"));
  await assert.rejects(exportNativeSession("claude", f.id, f.cwd, f.home), /symbolic links/);
});

test("import rejects tampering, traversal, credentials, duplicates, source UUID and existing destinations before writes", async (t) => {
  const f = await fixture("codex"); t.after(() => rm(f.root, { recursive: true, force: true }));
  const bundle = await exportNativeSession("codex", f.id, f.cwd, f.home);
  for (const entry of [file("project", "../escape", "bad"), file("project", "/absolute", "bad"), file("project", "C:\\auth.json", "bad"), file("project", ".claude/settings.json", "bad"), file("session", "auth.json", "bad")]) {
    const destination = join(f.root, randomUUID());
    await assert.rejects(importNativeSession({ ...bundle, files: [...bundle.files, entry] }, { directory: destination }, f.home));
    await assert.rejects(lstat(destination), { code: "ENOENT" });
  }
  await assert.rejects(importNativeSession({ ...bundle, files: [...bundle.files, bundle.files[0]] }, { directory: join(f.root, "duplicate") }, f.home), /Duplicate/);
  const tampered: SessionBundle = structuredClone(bundle); tampered.files[0].sha256 = "0".repeat(64);
  await assert.rejects(importNativeSession(tampered, { directory: join(f.root, "tampered") }, f.home), /hash/);
  await assert.rejects(importNativeSession(bundle, { directory: join(f.root, "same"), sessionId: f.id }, f.home), /fresh/);
  await assert.rejects(importNativeSession(bundle, { directory: f.cwd }, f.home), /new or empty/);
  await symlink(f.cwd, join(f.root, "link"));
  await assert.rejects(importNativeSession(bundle, { directory: join(f.root, "link") }, f.home), /new or empty/);
});

test("Claude refuses native UUID collisions and restores into an empty caller directory", async (t) => {
  const f = await fixture("claude"); t.after(() => rm(f.root, { recursive: true, force: true }));
  const bundle = await exportNativeSession("claude", f.id, f.cwd, f.home);
  const duplicate = randomUUID(); await writeFile(join(dirname(f.source), `${duplicate}.jsonl`), "{}\n");
  await assert.rejects(importNativeSession(bundle, { directory: join(f.root, "new"), sessionId: duplicate }, f.home), /already exists/);
  const destination = join(f.root, "empty"); await mkdir(destination); await chmod(destination, 0o755);
  const imported = await importNativeSession(bundle, { directory: destination }, f.home);
  assert.notEqual(imported.sessionId, f.id);
  assert.equal((await lstat(destination)).mode & 0o777, 0o700);
});

test("offline CLI exports/imports with no UI, login or provider process and clean stdout JSON", async (t) => {
  const f = await fixture("claude"); t.after(() => rm(f.root, { recursive: true, force: true }));
  const env = hermeticEnv({ CLAUDE_CONFIG_DIR: f.home, CODEX_HOME: join(f.root, "unused") });
  const run = (args: string[], input?: string) => execFileSync(join(launcher, "scripts/session.sh"), args, { env, encoding: "utf8", input });
  const bundle = run(["export", "--harness", "claude", "--session-id", f.id, "--cwd", f.cwd]);
  assert.equal(JSON.parse(bundle).sessionId, f.id);
  const result = JSON.parse(run(["import", "--directory", join(f.root, "from-stdin")], bundle));
  assert.notEqual(result.sessionId, f.id);
  const path = join(f.root, "bundle.json"); await writeFile(path, bundle);
  const fromFile = JSON.parse(run(["import", "--directory", join(f.root, "from-file"), "--input", path]));
  assert.notEqual(fromFile.sessionId, result.sessionId);
});
