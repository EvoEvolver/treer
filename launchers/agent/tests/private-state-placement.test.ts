import assert from "node:assert/strict";
import { test } from "node:test";
import { chmodSync, mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { privateStatePlacement } from "../src/private-state-placement.js";

test("private-state placement mirrors Core: physical paths, all ancestors and fresh VCS checks", t => {
  const base = mkdtempSync(join(realpathSync.native(tmpdir()), "private-placement-"));
  t.after(() => rmSync(base, { recursive: true, force: true }));
  const home = join(base, "home"), privateRoot = join(home, ".local", "state", "agent"), project = join(base, "project");
  mkdirSync(privateRoot, { recursive: true }); mkdirSync(project);
  assert.equal(privateStatePlacement(home, privateRoot), "available");
  assert.equal(privateStatePlacement(project, privateRoot), "available");
  assert.equal(privateStatePlacement(privateRoot, privateRoot), "agent_private_state_inside_cwd");
  assert.equal(privateStatePlacement(base, privateRoot), "agent_private_state_inside_cwd", "a later dot component does not suffice");
  assert.equal(privateStatePlacement(home, "relative"), "agent_private_state_unavailable");
  const visible = join(home, "state", "agent"); mkdirSync(visible, { recursive: true });
  assert.equal(privateStatePlacement(home, visible), "agent_private_state_inside_cwd");
  for (const directory of [privateRoot, join(home, ".local"), home, base]) {
    for (const marker of [".git", ".hg", ".jj", ".svn", ".bzr", ".sl"]) {
      const path = join(directory, marker); mkdirSync(path);
      assert.equal(privateStatePlacement(home, privateRoot), "agent_private_state_inside_vcs_worktree");
      assert.equal(privateStatePlacement(project, privateRoot), "agent_private_state_inside_vcs_worktree", "VCS check is independent of cwd containment");
      rmSync(path, { recursive: true });
      assert.equal(privateStatePlacement(home, privateRoot), "available", "no cached verdict");
    }
  }
  writeFileSync(join(home, ".git"), "gitdir: elsewhere");
  assert.equal(privateStatePlacement(home, privateRoot), "agent_private_state_inside_vcs_worktree");
  rmSync(join(home, ".git"));
});

test("private-state placement resolves aliases and rejects dangling marker links", { skip: process.platform === "win32" ? "POSIX aliases; Windows junctions covered separately" : false }, t => {
  const base = mkdtempSync(join(realpathSync.native(tmpdir()), "private-alias-"));
  t.after(() => rmSync(base, { recursive: true, force: true }));
  const home = join(base, "home"), privateRoot = join(home, ".state", "agent"); mkdirSync(privateRoot, { recursive: true });
  const alias = join(base, "alias"); symlinkSync(home, alias);
  assert.equal(privateStatePlacement(alias, privateRoot), "available");
  symlinkSync(join(base, "missing"), join(home, ".hg"));
  assert.equal(privateStatePlacement(alias, privateRoot), "agent_private_state_inside_vcs_worktree");
  const rootAlias = join(base, "root-alias"); symlinkSync(privateRoot, rootAlias);
  assert.equal(privateStatePlacement(base, rootAlias), "agent_private_state_inside_vcs_worktree", "private alias cannot hide repository ancestry");
});

test("private-state placement accepts native Windows junction aliases", { skip: process.platform !== "win32" }, t => {
  const base = mkdtempSync(join(realpathSync.native(tmpdir()), "private-junction-"));
  t.after(() => rmSync(base, { recursive: true, force: true }));
  const home = join(base, "home"), privateRoot = join(home, ".state", "agent"); mkdirSync(privateRoot, { recursive: true });
  const alias = join(base, "alias"); symlinkSync(home, alias, "junction");
  assert.equal(privateStatePlacement(alias, privateRoot), "available");
  mkdirSync(join(home, ".git"));
  assert.equal(privateStatePlacement(alias, privateRoot), "agent_private_state_inside_vcs_worktree");
});

test("private-state marker inspection failures refuse storage", { skip: process.platform === "win32" || process.getuid?.() === 0 }, t => {
  const base = mkdtempSync(join(realpathSync.native(tmpdir()), "private-permission-"));
  t.after(() => rmSync(base, { recursive: true, force: true }));
  const home = join(base, "home"), privateRoot = join(home, ".state"); mkdirSync(privateRoot, { recursive: true });
  chmodSync(privateRoot, 0);
  try { assert.equal(privateStatePlacement(home, privateRoot), "agent_private_state_inside_vcs_worktree"); }
  finally { chmodSync(privateRoot, 0o700); }
});
