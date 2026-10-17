// Release data validation/hash oracle. No installation or command expansion.
import { createHash } from "node:crypto";

const platforms = new Set(["darwin-aarch64", "darwin-x86_64", "linux-aarch64", "linux-x86_64", "windows-x86_64"]);
const providers = {
  codex: { package: "@openai/codex", names: ["codex"] },
  claude: { package: "@anthropic-ai/claude-code", names: ["claude"] },
  gemini: { package: "@google/gemini-cli", names: ["gemini"] },
  opencode: { package: "opencode-ai", names: ["opencode"] },
  cursor: { names: ["agent", "cursor-agent"] },
  grok: { names: ["grok"] },
};
function fail() { throw new Error("provider_cli_table_invalid"); }
function object(value, required, optional = []) {
  if (!value || typeof value !== "object" || Array.isArray(value)
    || required.some(key => !Object.hasOwn(value, key))
    || Object.keys(value).some(key => !required.includes(key) && !optional.includes(key))) fail();
}
function text(value, max = 1024) {
  if (typeof value !== "string" || !value.length || value.length > max || /[\u0000-\u001f\u007f-\u009f]/u.test(value)) fail();
}
function list(value) {
  if (!Array.isArray(value) || !value.length || new Set(value).size !== value.length) fail();
}
function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object") return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
  return JSON.stringify(value);
}
const sha256 = bytes => createHash("sha256").update(bytes).digest("hex");
export function recipeHash(recipe) {
  const { recipe_hash: ignored, ...data } = recipe;
  return sha256(canonical(data));
}

export function providerRecipes(bytes) {
  let table;
  try { table = JSON.parse(bytes.toString()); } catch { fail(); }
  object(table, ["schema", "schema_version", "recipes"]);
  if (table.schema !== "treer.provider-recipes/v1" || table.schema_version !== 1) fail();
  list(table.recipes);
  const ids = new Set(), coverage = new Set();
  for (const recipe of table.recipes) {
    const base = ["provider_id", "recipe_id", "recipe_version", "platforms", "binary_names", "supported", "official_docs", "login_hint"];
    object(recipe, base, recipe?.supported === true ? ["node_min", "install", "check", "lifecycle_review"] : ["unsupported_reason"]);
    if (!Object.hasOwn(providers, recipe.provider_id) || typeof recipe.supported !== "boolean") fail();
    const known = providers[recipe.provider_id];
    text(recipe.recipe_id, 80);
    if (!/^[a-z][a-z0-9-]+$/.test(recipe.recipe_id) || ids.has(recipe.recipe_id)) fail();
    ids.add(recipe.recipe_id);
    if (!Number.isSafeInteger(recipe.recipe_version) || recipe.recipe_version < 1) fail();
    list(recipe.platforms); list(recipe.binary_names);
    if (recipe.binary_names.some(name => !known.names.includes(name))) fail();
    for (const platform of recipe.platforms) {
      if (!platforms.has(platform) || coverage.has(`${recipe.provider_id}/${platform}`)) fail();
      coverage.add(`${recipe.provider_id}/${platform}`);
    }
    text(recipe.official_docs); text(recipe.login_hint);
    let url;
    try { url = new URL(recipe.official_docs); } catch { fail(); }
    if (url.protocol !== "https:" || url.username || url.password) fail();
    if (!recipe.supported) {
      text(recipe.unsupported_reason, 80);
      if (!/^[a-z][a-z0-9_]+$/.test(recipe.unsupported_reason)) fail();
      continue;
    }
    if (recipe.node_min !== 22 || recipe.platforms.includes("windows-x86_64")) fail();
    object(recipe.install, ["kind", "package", "version", "integrity", "registry", "ignore_scripts"]);
    const install = recipe.install;
    if (install.kind !== "npm-global" || !known.package || install.package !== known.package
      || typeof install.version !== "string" || !/^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/.test(install.version)
      || install.registry !== "https://registry.npmjs.org/" || typeof install.ignore_scripts !== "boolean") fail();
    if (typeof install.integrity !== "string" || !/^sha512-[A-Za-z0-9+/]{86}==$/.test(install.integrity)) fail();
    const digest = install.integrity.slice(7);
    if (Buffer.from(digest, "base64").length !== 64 || Buffer.from(digest, "base64").toString("base64") !== digest) fail();
    object(recipe.check, ["executable", "args"]);
    if (!recipe.binary_names.includes(recipe.check.executable) || !Array.isArray(recipe.check.args)
      || recipe.check.args.length !== 1 || !["--version", "version"].includes(recipe.check.args[0])) fail();
    if (!install.ignore_scripts) {
      object(recipe.lifecycle_review, ["reviewed_at", "reviewer", "files"]);
      const review = recipe.lifecycle_review;
      if (typeof review.reviewed_at !== "string" || !/^\d{4}-\d{2}-\d{2}$/.test(review.reviewed_at)) fail();
      text(review.reviewer, 80); list(review.files);
      for (const file of review.files) {
        object(file, ["member", "sha256"]);
        if (typeof file.member !== "string" || !/^package\/[a-zA-Z0-9_./-]+$/.test(file.member) || file.member.split("/").some(part => !part || part === "." || part === "..")
          || typeof file.sha256 !== "string" || !/^[a-f0-9]{64}$/.test(file.sha256)) fail();
      }
    } else if (Object.hasOwn(recipe, "lifecycle_review")) fail();
  }
  for (const provider of Object.keys(providers)) for (const platform of platforms) if (!coverage.has(`${provider}/${platform}`)) fail();
  return { table, table_hash: sha256(bytes), recipe_hashes: Object.fromEntries(table.recipes.map(recipe => [recipe.recipe_id, recipeHash(recipe)])) };
}
