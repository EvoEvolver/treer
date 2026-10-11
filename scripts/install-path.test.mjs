import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

const script = readFileSync(new URL("../crates/treer-proxy/src/install-path.sh", import.meta.url), "utf8");

function fixture(t, shell = "/bin/zsh", suffix = ".local/bin") {
  const home = mkdtempSync(join(tmpdir(), "treer-path-test-"));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const directory = join(home, suffix);
  mkdirSync(directory, { recursive: true });
  for (const binary of ["treer", "treer-agent-server"]) {
    writeFileSync(join(directory, binary), "#!/bin/sh\nprintf '%s\\n' installed\n");
    chmodSync(join(directory, binary), 0o755);
  }
  const env = { ...process.env, HOME: home, SHELL: shell, PATH: "/usr/bin:/bin", TREER_INSTALL_MODIFY_PATH: "1", install_dir: directory };
  delete env.ZDOTDIR;
  function install(extra = {}) {
    const result = spawnSync("sh", ["-eu"], { input: script, env: { ...env, ...extra }, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
    return result.stdout;
  }
  function startShell(executable, args, extra = {}) {
    const result = spawnSync(executable, args, { env: { ...env, ...extra }, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /installed\ninstalled/);
  }
  return { home, directory, env, install, startShell };
}

test("zsh login and interactive shells find both commands after installing twice", (t) => {
  const f = fixture(t);
  if (spawnSync("zsh", ["--version"]).error) { t.skip("zsh is unavailable"); return; }
  const original = "# existing user configuration\nexport USER_SETTING=preserved";
  writeFileSync(join(f.home, ".zshrc"), original);
  f.install();
  const first = readFileSync(join(f.home, ".zshrc"), "utf8");
  f.install();
  assert.equal(readFileSync(join(f.home, ".zshrc"), "utf8"), first);
  assert.ok(first.startsWith(original + "\n"));
  for (const option of ["-lc", "-ic"]) {
    f.startShell("zsh", [option, "treer; treer-agent-server"]);
  }
});

test("zsh respects ZDOTDIR", (t) => {
  const f = fixture(t);
  const zdotdir = join(f.home, "shell configuration");
  f.install({ ZDOTDIR: zdotdir });
  assert.ok(existsSync(join(zdotdir, ".zprofile")));
  assert.ok(existsSync(join(zdotdir, ".zshrc")));
  assert.ok(!existsSync(join(f.home, ".zshrc")));
});

test("bash preserves its existing login file and supports interactive shells", (t) => {
  const f = fixture(t, "/bin/bash");
  const original = "export USER_SETTING=preserved\n";
  writeFileSync(join(f.home, ".profile"), original);
  f.install();
  assert.ok(!existsSync(join(f.home, ".bash_profile")));
  assert.ok(readFileSync(join(f.home, ".profile"), "utf8").startsWith(original));
  // Source the selected login file explicitly to avoid system /etc/profile PATH rewrites.
  f.startShell("bash", ["-c", '. "$HOME/.profile"; treer; treer-agent-server']);
  f.startShell("bash", ["--noprofile", "-ic", "treer; treer-agent-server"]);
});

test("custom installation paths are quoted as data", (t) => {
  const f = fixture(t, "/bin/sh", "bin with 'quotes' $variables `commands` $(commands)");
  f.install();
  f.startShell("sh", ["-c", '. "$HOME/.profile"; treer; treer-agent-server']);
  f.install();
  assert.equal((readFileSync(join(f.home, ".profile"), "utf8").match(/# Treer CLI and Controller/g) || []).length, 1);
});

test("opt-out and unsupported shells leave startup files untouched", (t) => {
  for (const shell of ["/bin/zsh", "/bin/fish"]) {
    const f = fixture(t, shell);
    f.install(shell.endsWith("zsh") ? { TREER_INSTALL_MODIFY_PATH: "0" } : {});
    assert.ok(readdirSync(f.home).every((file) => ![".profile", ".zprofile", ".zshrc", ".bashrc", ".bash_profile"].includes(file)));
  }
});

test("PATH setup never inserts the directory twice in a running shell", (t) => {
  const f = fixture(t, "/bin/sh");
  f.install();
  const result = spawnSync("sh", ["-c", '. "$HOME/.profile"; . "$HOME/.profile"; printf "%s" "$PATH"'], { env: f.env, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, `${f.directory}:/usr/bin:/bin`);
});
