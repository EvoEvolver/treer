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
  assert.equal(source.recipes.filter((recipe: any) => recipe.supported).length, 2);
  for (const recipe of source.recipes) {
    if (!recipe.supported) {
      assert.equal(recipe.install, undefined); assert.equal(recipe.check, undefined); continue;
    }
    assert.equal(recipe.platforms.includes("windows-x86_64"), false);
    const snapshot = review.packages.find((p: any) => p.name === recipe.install.package);
    assert.equal(recipe.install.version, snapshot.version);
    assert.equal(recipe.install.integrity, snapshot.integrity);
    assert.equal(recipe.install.ignore_scripts, true);
    assert.deepEqual(snapshot.scripts, {});
  }
});

test("invalid release data fails closed before payload construction", () => {
  const mutations: Array<(table: any) => void> = [
    t => { t.recipes[0] = null; },
    t => { t.schema_version = 2; },
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

test("lifecycle opt-in requires a bounded review record and changes the consent hash", () => {
  const changed = structuredClone(source), recipe = changed.recipes[0];
  recipe.install.ignore_scripts = false;
  recipe.lifecycle_review = { reviewed_at: "2026-10-16", reviewer: "fixture-reviewer", files: [{ member: "package/install.cjs", sha256: "a".repeat(64) }] };
  assert.notEqual(providerRecipes(encode(changed)).recipe_hashes[recipe.recipe_id], providerRecipes(bytes).recipe_hashes[recipe.recipe_id]);
  recipe.lifecycle_review.files[0].member = "package/../escape";
  assert.throws(() => providerRecipes(encode(changed)), /provider_cli_table_invalid/);
});
