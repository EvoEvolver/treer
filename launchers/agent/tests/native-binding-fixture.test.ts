import assert from "node:assert/strict";
import { chmodSync, copyFileSync, cpSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";
import { nativeBindingFixture } from "./native-binding-fixture.mjs";
import { nativeRuntime } from "../scripts/native-runtime.mjs";

test("native control fixtures isolate shared input paths without weakening runtime trust", { skip: process.platform === "win32" }, t => {
  const root = realpathSync.native(mkdtempSync(join(tmpdir(), "native-input-isolation-")));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const shared = join(root, "shared"), owned = join(root, "owned");
  mkdirSync(shared); chmodSync(shared, 0o775); mkdirSync(owned, { mode: 0o700 });
  const executable = join(shared, "node");
  copyFileSync(realpathSync.native(process.execPath), executable); chmodSync(executable, 0o755);
  const publicDirectory = join(shared, "public");
  cpSync(resolve(import.meta.dirname, "../public"), publicDirectory, { recursive: true });
  const original = readFileSync(executable);
  const binding = nativeBindingFixture(owned, publicDirectory, executable);
  const manifest = join(owned, "native-runtime.json");
  const save = () => writeFileSync(manifest, JSON.stringify(binding.value));
  save();
  assert.doesNotThrow(() => nativeRuntime(owned));
  assert.notEqual(binding.value.executable.path, executable);
  assert.notEqual(binding.value.public_dir, publicDirectory);
  assert.equal(lstatSync(shared).mode & 0o777, 0o775);
  assert.deepEqual(readFileSync(executable), original);
  assert.deepEqual(readFileSync(binding.value.executable.path), original);
  // Original shared inputs still fail the production guard. The fixture must
  // copy inputs, never chmod a shared runner/toolchain or relax nativeRuntime.
  const staged = binding.value.executable.path;
  binding.value.executable.path = executable; save();
  assert.throws(() => nativeRuntime(owned), /no Node fallback/);
  binding.value.executable.path = staged; save();
  const stagedPublic = binding.value.public_dir;
  binding.value.public_dir = publicDirectory; save();
  assert.throws(() => nativeRuntime(owned), /no Node fallback/);
  binding.value.public_dir = stagedPublic; save();
  writeFileSync(staged, "changed");
  assert.throws(() => nativeRuntime(owned), /no Node fallback/);
});
