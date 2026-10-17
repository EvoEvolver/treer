// Human-only provider CLI API consumer: no browser-generated installer argv.
export const PROVIDER_CLI_CAPABILITY = "machine.provider-cli-install.v1"
export const providerCliBase = (workspaceId: string, machineId: string) =>
  `/api/workspaces/${encodeURIComponent(workspaceId)}/machines/${encodeURIComponent(machineId)}/provider-clis`
export type ProviderCliStatus = "queued" | "running" | "succeeded" | "failed" | "timed_out" | "unknown"
export const providerCliTerminal = (status: ProviderCliStatus) => status !== "queued" && status !== "running"
export interface ProviderCliRecipeIdentity { recipe_id: string; recipe_version: number; recipe_hash: string }
export interface ProviderCliStepResult { step: number; exit_code: number | null; stdout: string; stderr: string; truncated: boolean; duration_ms: number }
export interface ProviderCliOperation extends ProviderCliRecipeIdentity {
  operation_id: string; machine_id: string; status: ProviderCliStatus; step: number | null
  elapsed_ms: number; exit_code: number | null; timed_out: boolean
  stdout: string; stderr: string; truncated: boolean; error_code?: string; steps?: ProviderCliStepResult[]
}
const statuses: readonly string[] = ["queued", "running", "succeeded", "failed", "timed_out", "unknown"]
const record = (v: unknown): v is Record<string, unknown> => Boolean(v && typeof v === "object" && !Array.isArray(v))
const integer = (v: unknown): v is number => Number.isSafeInteger(v)
const nonnegative = (v: unknown): v is number => integer(v) && v >= 0
const exit = (v: unknown): v is number | null => v === null || integer(v)
const step = (v: unknown): v is number => integer(v) && v >= 1 && v <= 5
const tail = (v: unknown): v is string => typeof v === "string" && v.length <= 32768
const code = (v: unknown): v is string => typeof v === "string" && /^[a-z][a-z0-9_]{0,127}$/.test(v)
// Scope every poll response to the operation/machine/recipe the human chose.
// Project known fields so accidental token fields never enter view state.
export function providerCliOperation(v: unknown, expected: ProviderCliRecipeIdentity & { operation_id: string; machine_id: string }): ProviderCliOperation | null {
  if (!record(v) || v.operation_id !== expected.operation_id || v.machine_id !== expected.machine_id ||
    v.recipe_id !== expected.recipe_id || v.recipe_version !== expected.recipe_version || v.recipe_hash !== expected.recipe_hash ||
    typeof v.status !== "string" || !statuses.includes(v.status) || (v.step != null && !step(v.step)) ||
    !nonnegative(v.elapsed_ms) || !exit(v.exit_code) || typeof v.timed_out !== "boolean" ||
    !tail(v.stdout) || !tail(v.stderr) || typeof v.truncated !== "boolean" || (v.error_code !== undefined && !code(v.error_code))) return null
  if ((v.status === "queued" || v.status === "running") && (v.stdout !== "" || v.stderr !== "" || v.steps !== undefined)) return null
  if (v.status === "succeeded" && (v.exit_code !== 0 || v.timed_out)) return null
  let steps: ProviderCliStepResult[] | undefined
  if (v.steps !== undefined) {
    if (!Array.isArray(v.steps) || v.steps.length > 5) return null
    steps = []
    let last = 0
    for (const item of v.steps) {
      if (!record(item) || !step(item.step) || item.step <= last || !exit(item.exit_code) || !tail(item.stdout) || !tail(item.stderr) ||
        typeof item.truncated !== "boolean" || !nonnegative(item.duration_ms)) return null
      last = item.step
      steps.push({ step: item.step, exit_code: item.exit_code, stdout: item.stdout, stderr: item.stderr, truncated: item.truncated, duration_ms: item.duration_ms })
    }
  }
  return { ...expected, status: v.status as ProviderCliStatus, step: (v.step ?? null) as number | null, elapsed_ms: v.elapsed_ms,
    exit_code: v.exit_code, timed_out: v.timed_out, stdout: v.stdout, stderr: v.stderr, truncated: v.truncated,
    ...(v.error_code === undefined ? {} : { error_code: v.error_code as string }), ...(steps === undefined ? {} : { steps }) }
}
// Offer installation only for one exact supported server-declared binary, with
// a proven removed identity on the machine still selected in the draft.
export function matchingProviderRecipe<T extends { supported: boolean; binary_names: string[] }>(
  recipes: readonly T[], command: string | undefined, creationState: string, failureMachineId: string, selectedMachineId: string,
): T | null {
  if (creationState !== "identity_removed" || failureMachineId !== selectedMachineId ||
    !command || !/^[A-Za-z0-9_-][A-Za-z0-9._-]{0,127}$/.test(command) || command.endsWith(".")) return null
  const matches = recipes.filter(recipe => recipe.supported === true && recipe.binary_names.includes(command))
  return matches.length === 1 ? matches[0] : null
}

// Mirrors crates/treer-protocol/src/provider_cli.rs. Release recipe data and
// execution remain Core/launcher-owned; this validates only display envelopes.
export type ProviderCliPlanStep =
  | { step: number; kind: "command"; purpose: string; executable: string; args: string[] }
  | { step: number; kind: "verify"; purpose: string; algorithm: string; expected: string }
  | { step: number; kind: "resolve"; purpose: string; rule: string; binary: string; args: string[]; prefix_from_step: number }
export interface ProviderCliRecipe extends ProviderCliRecipeIdentity {
  provider_id: string; platform: string; binary_names: string[]; supported: boolean; official_docs: string; login_hint: string
  unsupported_reason?: string; node_min?: number; lifecycle_scripts?: { enabled: boolean }
  package?: { name: string; version: string; integrity: string; registry: string }; plan?: ProviderCliPlanStep[]
}
export interface ProviderCliRecipes { schema_version: number; machine_id: string; platform: string; controller_source_sha: string; table_hash: string; recipes: ProviderCliRecipe[] }
export interface ProviderCliPrepared { confirmation_token: string; expires_at: string; machine_id: string; table_hash: string; recipe: ProviderCliRecipe }
const text = (v: unknown, max = 8192): v is string => typeof v === "string" && v.length > 0 && v.length <= max
const hash = (v: unknown): v is string => typeof v === "string" && /^[a-f0-9]{64}$/.test(v)
const slug = (v: unknown): v is string => typeof v === "string" && /^[a-z0-9][a-z0-9_-]{0,127}$/.test(v)
const strings = (v: unknown): v is string[] => Array.isArray(v) && v.length <= 64 && v.every(item => text(item))
export function providerCliPlan(v: unknown): ProviderCliPlanStep[] | null {
  if (!Array.isArray(v) || v.length !== 5) return null
  const plan: ProviderCliPlanStep[] = []
  for (const [i, item] of v.entries()) {
    if (!record(item) || item.step !== i + 1 || !text(item.purpose, 128)) return null
    if (item.kind === "command" && text(item.executable) && strings(item.args)) plan.push({ step: item.step, kind: item.kind, purpose: item.purpose, executable: item.executable, args: item.args })
    else if (item.kind === "verify" && item.step === 2 && item.algorithm === "sha512" && text(item.expected, 256)) plan.push({ step: item.step, kind: item.kind, purpose: item.purpose, algorithm: item.algorithm, expected: item.expected })
    else if (item.kind === "resolve" && item.step === 5 && item.prefix_from_step === 4 && text(item.rule) && text(item.binary, 128) && strings(item.args)) plan.push({ step: item.step, kind: item.kind, purpose: item.purpose, rule: item.rule, binary: item.binary, args: item.args, prefix_from_step: 4 })
    else return null
  }
  return plan
}
export function providerCliRecipe(v: unknown, platform: string): ProviderCliRecipe | null {
  if (!record(v) || !slug(v.provider_id) || !slug(v.recipe_id) || !integer(v.recipe_version) || v.recipe_version < 1 || !hash(v.recipe_hash) ||
    v.platform !== platform || !strings(v.binary_names) || !v.binary_names.length || !v.binary_names.every(item => /^[A-Za-z0-9_-][A-Za-z0-9._-]{0,127}$/.test(item)) ||
    typeof v.supported !== "boolean" || !text(v.official_docs) || !text(v.login_hint)) return null
  const base = { provider_id: v.provider_id, recipe_id: v.recipe_id, recipe_version: v.recipe_version, recipe_hash: v.recipe_hash, platform,
    binary_names: v.binary_names, supported: v.supported, official_docs: v.official_docs, login_hint: v.login_hint }
  if (!v.supported) return code(v.unsupported_reason) && v.plan === undefined && v.package === undefined
    ? { ...base, unsupported_reason: v.unsupported_reason } : null
  const plan = providerCliPlan(v.plan)
  if (!plan || !record(v.package) || !text(v.package.name, 256) || !text(v.package.version, 128) || !text(v.package.integrity, 256) ||
    !text(v.package.registry) || !record(v.lifecycle_scripts) || typeof v.lifecycle_scripts.enabled !== "boolean" || !integer(v.node_min)) return null
  return { ...base, node_min: v.node_min, lifecycle_scripts: { enabled: v.lifecycle_scripts.enabled },
    package: { name: v.package.name, version: v.package.version, integrity: v.package.integrity, registry: v.package.registry }, plan }
}
export function providerCliRecipes(v: unknown, machineId: string): ProviderCliRecipes | null {
  if (!record(v) || v.schema_version !== 1 || v.machine_id !== machineId || !text(v.platform, 128) || !text(v.controller_source_sha, 128) ||
    !hash(v.table_hash) || !Array.isArray(v.recipes) || v.recipes.length > 64) return null
  const recipes: ProviderCliRecipe[] = []
  for (const item of v.recipes) {
    const recipe = providerCliRecipe(item, v.platform)
    if (!recipe || recipes.some(other => other.recipe_id === recipe.recipe_id)) return null
    recipes.push(recipe)
  }
  return { schema_version: 1, machine_id: machineId, platform: v.platform, controller_source_sha: v.controller_source_sha, table_hash: v.table_hash, recipes }
}
export function providerCliPrepared(v: unknown, recipes: ProviderCliRecipes, selected: ProviderCliRecipe): ProviderCliPrepared | null {
  if (!record(v) || v.machine_id !== recipes.machine_id || v.table_hash !== recipes.table_hash || !text(v.confirmation_token, 512) ||
    !text(v.expires_at, 128) || !Number.isFinite(Date.parse(v.expires_at))) return null
  const recipe = providerCliRecipe(v.recipe, recipes.platform)
  if (!recipe?.supported || recipe.recipe_id !== selected.recipe_id || recipe.recipe_version !== selected.recipe_version || recipe.recipe_hash !== selected.recipe_hash) return null
  // Always show the freshly prepared plan, including its machine-resolved paths.
  return { confirmation_token: v.confirmation_token, expires_at: v.expires_at, machine_id: recipes.machine_id, table_hash: recipes.table_hash, recipe }
}
export const providerCliRef = (recipe: ProviderCliRecipeIdentity): ProviderCliRecipeIdentity => ({ recipe_id: recipe.recipe_id, recipe_version: recipe.recipe_version, recipe_hash: recipe.recipe_hash })
