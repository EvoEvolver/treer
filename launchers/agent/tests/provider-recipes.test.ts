import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { providerRecipes, recipeHash } from "../scripts/provider-recipes.mjs";

const bytes = readFileSync(new URL("../provider-recipes.json", import.meta.url));
const source = JSON.parse(bytes.toString());
const encode = (data: unknown) => Buffer.from(JSON.stringify(data));

test("release recipe bytes and canonical hashes match the cross-language golden fixture", () => {
  const golden = JSON.parse(readFileSync(new URL("./provider-recipe-hashes.json", import.meta.url), "utf8"));
  const result = providerRecipes(bytes);
  assert.deepEqual({ table_hash: result.table_hash, recipe_hashes: result.recipe_hashes }, golden);
  const reordered = { ...source, recipes: source.recipes.map((recipe: any) => Object.fromEntries(Object.entries(recipe).reverse())) };
  const changedWhitespace = providerRecipes(encode(reordered));
  assert.notEqual(changedWhitespace.table_hash, result.table_hash);
  assert.deepEqual(changedWhitespace.recipe_hashes, result.recipe_hashes);
  assert.equal(recipeHash({ ...source.recipes[0], recipe_hash: "excluded" }), recipeHash(source.recipes[0]));
  const edited = structuredClone(source.recipes[0]); edited.install.integrity = "sha512-" + Buffer.alloc(64, 1).toString("base64");
  assert.notEqual(recipeHash(edited), recipeHash(source.recipes[0]));
});

test("known providers cover all five platforms with explicit unsupported entries and matching reviewed metadata", () => {
  const review = JSON.parse(readFileSync(new URL("../provider-review.json", import.meta.url), "utf8"));
  providerRecipes(bytes);
  assert.equal(source.recipes.filter((recipe: any) => recipe.supported).length, 3);
  for (const recipe of source.recipes) {
    if (!recipe.supported) {
      assert.equal(recipe.install, undefined); assert.equal(recipe.check, undefined); continue;
    }
    assert.equal(recipe.platforms.includes("windows-x86_64"), false);
    const snapshot = review.packages.find((p: any) => p.name === recipe.install.package);
    assert.equal(recipe.install.version, snapshot.version);
    assert.equal(recipe.install.integrity, snapshot.integrity);
    assert.equal(recipe.expected_version, recipe.install.version);
    if (recipe.provider_id === "claude") {
      assert.equal(recipe.install.ignore_scripts, false);
      assert.equal(recipe.install.omit_optional, false);
      assert.deepEqual(recipe.lifecycle_review.scripts, snapshot.scripts);
      assert.equal(recipe.lifecycle_review.files.find((f: any) => f.member === snapshot.lifecycle.member).sha256, snapshot.lifecycle.sha256);
    } else {
      assert.equal(recipe.install.ignore_scripts, true);
      assert.deepEqual(snapshot.scripts, {});
    }
  }
});

test("invalid release data fails closed before payload construction", () => {
  const mutations: Array<(table: any) => void> = [
    t => { t.recipes[0] = null; },
    t => { t.schema_version = 1; },
    t => { t.recipes[0].provider_id = "constructor"; },
    t => { t.recipes[0].platforms.push("windows-x86_64"); },
    t => { t.recipes[0].platforms[0] = "unknown"; },
    t => { t.recipes.push(structuredClone(t.recipes[0])); },
    t => { t.recipes[0].install.args = ["--userconfig=/outside"]; },
    t => { t.recipes[0].install.package = "unreviewed-package"; },
    t => { t.recipes[0].install.version = "latest"; },
    t => { t.recipes[0].install.version = "^0.163.0"; },
    t => { delete t.recipes[0].install.integrity; },
    t => { t.recipes[0].install.integrity = "sha512-invalid"; },
    t => { t.recipes[0].install.registry = "https://foreign.example/"; },
    t => { t.recipes[0].install.ignore_scripts = false; },
    t => { t.recipes[0].check.executable = "sh"; },
    t => { t.recipes[0].check.args = ["--eval", "untrusted"]; },
    t => { t.recipes[1].install = t.recipes[0].install; },
    t => { t.recipes[1].unsupported_reason = "reason\nspoof"; },
    t => { t.recipes[0].binary_names = [""]; },
    t => { t.recipes[0].official_docs = "https://username:secret@example.com/"; },
  ];
  for (const mutate of mutations) {
    const changed = structuredClone(source); mutate(changed);
    assert.throws(() => providerRecipes(encode(changed)), /^Error: provider_cli_table_invalid$/);
  }
});

test("Claude has complete native pins and only the reviewed postinstall enabled", () => {
  const recipe = source.recipes.find((r: any) => r.provider_id === "claude" && r.supported);
  assert.deepEqual(Object.keys(recipe.native_binaries).sort(), [...recipe.platforms].sort());
  assert.equal(recipe.binary_path, "bin/claude.exe");
  assert.deepEqual(recipe.lifecycle_review.runs, ["postinstall"]);
  assert.deepEqual(recipe.lifecycle_review.closure, { packages: 2, with_scripts: [recipe.install.package] });
  assert.equal(recipe.deadline_seconds, undefined, "release must measure before increasing the 300s default");
  assert.deepEqual(source.recipes.find((r: any) => r.recipe_id === "claude-native-unavailable").platforms, ["darwin-x86_64", "windows-x86_64"]);
  for (const mutate of [
    (r: any) => { r.native_binaries["darwin-aarch64"].sha256 = "b".repeat(64); },
    (r: any) => { r.install.ignore_scripts = true; },
    (r: any) => { r.install.omit_optional = true; },
    (r: any) => { r.expected_version = "2.1.303"; },
  ]) {
    const changed = structuredClone(recipe); mutate(changed);
    assert.notEqual(recipeHash(changed), recipeHash(recipe));
  }
});

test("native and lifecycle metadata reject incomplete, mismatched and unsafe inputs", () => {
  const mutations: Array<(recipe: any) => void> = [
    r => { delete r.expected_version; },
    r => { r.expected_version = "v2.1.302"; },
    r => { r.expected_version = "2.1.3020"; },
    r => { r.deadline_seconds = 59; },
    r => { r.deadline_seconds = 901; },
    r => { r.deadline_seconds = 60.5; },
    r => { r.binary_path = "../claude"; },
    r => { r.binary_path = "/bin/claude.exe"; },
    r => { delete r.native_binaries; },
    r => { delete r.native_binaries["linux-x86_64"]; },
    r => { r.native_binaries["windows-x86_64"] = r.native_binaries["linux-x86_64"]; },
    r => { r.native_binaries["linux-x86_64"].package = "@untrusted/native"; },
    r => { r.native_binaries["linux-x86_64"].version = "2.1.301"; },
    r => { r.native_binaries["linux-x86_64"].integrity = "sha512-invalid"; },
    r => { r.native_binaries["linux-x86_64"].member = "package/../claude"; },
    r => { r.native_binaries["linux-x86_64"].sha256 = "A".repeat(64); },
    r => { r.native_binaries["linux-x86_64"].size = 0; },
    r => { r.native_binaries["linux-x86_64"].size = 1.5; },
    r => { r.native_binaries["linux-x86_64"].unpacked_size = 1; },
    r => { r.native_binaries["linux-x86_64"].unpacked_size = 1024 ** 3 + 1; },
    r => { r.native_binaries["linux-x86_64"].unknown = true; },
    r => { delete r.install.omit_optional; },
    r => { r.install.omit_optional = true; },
    r => { r.install.ignore_scripts = true; },
    r => { delete r.lifecycle_review; },
    r => { r.lifecycle_review.files[0].member = "package/../escape"; },
    r => { r.lifecycle_review.files.pop(); },
    r => { r.lifecycle_review.files[1] = r.lifecycle_review.files[0]; },
    r => { r.lifecycle_review.scripts.postinstall = "node arbitrary.cjs"; },
    r => { r.lifecycle_review.scripts.install = "unreviewed"; },
    r => { r.lifecycle_review.runs.push("prepare"); },
    r => { r.lifecycle_review.closure.packages = 3; },
    r => { r.lifecycle_review.closure.with_scripts.push("@untrusted/native"); },
  ];
  for (const mutate of mutations) {
    const changed = structuredClone(source);
    mutate(changed.recipes.find((r: any) => r.provider_id === "claude" && r.supported));
    assert.throws(() => providerRecipes(encode(changed)), /^Error: provider_cli_table_invalid$/);
  }
  for (const deadline of [60, 300, 900]) {
    const changed = structuredClone(source);
    changed.recipes[0].deadline_seconds = deadline;
    providerRecipes(encode(changed));
  }
});
