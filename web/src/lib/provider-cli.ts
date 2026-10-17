// Human-only provider CLI API consumer: no browser-generated installer argv.
export const PROVIDER_CLI_CAPABILITY = "machine.provider-cli-install.v2"
export const PROVIDER_CLI_PLAN_CAPABILITY = "machine.provider-cli-plan.v2"
export type ProviderCliAction = "install" | "replace" | "noop"
export type ProviderCliRelation = "upgrade" | "downgrade" | "unknown" | "incomparable" | null
export interface ProviderCliObservation {
  state: "absent" | "installed" | "unknown"; current_version: string | null; version_text: string | null
  executable: string | null; found_in_prefix: boolean; node_arch?: string
  probe: { exit_code: number | null; timed_out: boolean; cleanup_confirmed: boolean; error_code: string | null }
}
export interface ProviderCliObservedSummary {
  state: ProviderCliObservation["state"]; current_version: string | null; found_in_prefix: boolean; observation_hash: string
}
export const providerCliNeedsAcknowledgement = (relation: ProviderCliRelation) =>
  relation === "downgrade" || relation === "unknown" || relation === "incomparable"
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
  prefix_modified?: boolean; verified_file?: { size?: number; sha256?: string; failure?: string }
  observed_before?: ProviderCliObservedSummary
}
const statuses: readonly string[] = ["queued", "running", "succeeded", "failed", "timed_out", "unknown"]
const record = (v: unknown): v is Record<string, unknown> => Boolean(v && typeof v === "object" && !Array.isArray(v))
const integer = (v: unknown): v is number => Number.isSafeInteger(v)
const nonnegative = (v: unknown): v is number => integer(v) && v >= 0
const exit = (v: unknown): v is number | null => v === null || integer(v)
const step = (v: unknown, max = 5): v is number => integer(v) && v >= 1 && v <= max
const tail = (v: unknown): v is string => typeof v === "string" && v.length <= 32768
const code = (v: unknown): v is string => typeof v === "string" && /^[a-z][a-z0-9_]{0,127}$/.test(v)
// Scope every poll response to the operation/machine/recipe the human chose.
// Project known fields so accidental token fields never enter view state.
export function providerCliOperation(v: unknown, expected: ProviderCliRecipeIdentity & { operation_id: string; machine_id: string; max_steps?: number; verified_pin?: { size: number; sha256: string } }): ProviderCliOperation | null {
  if (!record(v) || v.operation_id !== expected.operation_id || v.machine_id !== expected.machine_id ||
    v.recipe_id !== expected.recipe_id || v.recipe_version !== expected.recipe_version || v.recipe_hash !== expected.recipe_hash ||
    typeof v.status !== "string" || !statuses.includes(v.status) || (v.step != null && !step(v.step, expected.max_steps ?? 5) && !(v.step === 0 && v.status === "failed" && v.error_code === "provider_cli_state_changed")) ||
    !nonnegative(v.elapsed_ms) || !exit(v.exit_code) || typeof v.timed_out !== "boolean" ||
    !tail(v.stdout) || !tail(v.stderr) || typeof v.truncated !== "boolean" || (v.error_code !== undefined && !code(v.error_code))) return null
  if ((v.status === "queued" || v.status === "running") && (v.stdout !== "" || v.stderr !== "" || v.steps !== undefined)) return null
  if (v.status === "succeeded" && (v.exit_code !== 0 || v.timed_out)) return null
  let observed: ProviderCliObservedSummary | undefined
  if (v.observed_before != null) {
    if (!record(v.observed_before)) return null
    const value: Record<string, unknown> = { ...v.observed_before, current_version: v.observed_before.current_version ?? null }
    if (!record(value) || !observationState(value.state) || !nullableVersion(value.current_version) || typeof value.found_in_prefix !== "boolean" || !hash(value.observation_hash) ||
      value.state === "absent" && (value.found_in_prefix || value.current_version !== null) ||
      value.state === "installed" && (!value.found_in_prefix || !value.current_version)) return null
    observed = { state: value.state, current_version: value.current_version, found_in_prefix: value.found_in_prefix, observation_hash: value.observation_hash }
  }
  let steps: ProviderCliStepResult[] | undefined
  if (v.steps !== undefined) {
    if (!Array.isArray(v.steps) || v.steps.length > (expected.max_steps ?? 5)) return null
    steps = []
    let last = 0
    for (const item of v.steps) {
      if (!record(item) || !step(item.step, expected.max_steps ?? 5) || item.step <= last || !exit(item.exit_code) || !tail(item.stdout) || !tail(item.stderr) ||
        typeof item.truncated !== "boolean" || !nonnegative(item.duration_ms)) return null
      last = item.step
      steps.push({ step: item.step, exit_code: item.exit_code, stdout: item.stdout, stderr: item.stderr, truncated: item.truncated, duration_ms: item.duration_ms })
    }
  }
  if (v.step === 0 && steps?.length) return null
  if (v.prefix_modified !== undefined && typeof v.prefix_modified !== "boolean" || v.status === "succeeded" && v.prefix_modified === true) return null
  let verified: ProviderCliOperation["verified_file"]
  if (v.verified_file != null) {
    const f = v.verified_file
    if (!record(f) || f.size !== undefined && (!nonnegative(f.size) || f.size > 1073741824) || f.sha256 !== undefined && !hash(f.sha256) ||
      f.failure !== undefined && !["native_content", "entry_link", "not_regular", "path_refused"].includes(f.failure as string) ||
      f.failure === undefined && (f.size === undefined || f.sha256 === undefined) || v.status === "succeeded" && f.failure !== undefined || !providerCliTerminal(v.status as ProviderCliStatus)) return null
    verified = { ...(f.size === undefined ? {} : { size: f.size as number }), ...(f.sha256 === undefined ? {} : { sha256: f.sha256 as string }), ...(f.failure === undefined ? {} : { failure: f.failure as string }) }
  }
  if (expected.verified_pin && verified && !verified.failure && (verified.size !== expected.verified_pin.size || verified.sha256 !== expected.verified_pin.sha256)) return null
  if (expected.verified_pin && v.status === "succeeded" && (!verified || verified.failure)) return null
  return { operation_id: expected.operation_id, machine_id: expected.machine_id, ...providerCliRef(expected), ...(v.prefix_modified === undefined ? {} : { prefix_modified: v.prefix_modified as boolean }), ...(verified ? { verified_file: verified } : {}), status: v.status as ProviderCliStatus, step: (v.step ?? null) as number | null, elapsed_ms: v.elapsed_ms,
    exit_code: v.exit_code, timed_out: v.timed_out, stdout: v.stdout, stderr: v.stderr, truncated: v.truncated,
    ...(v.error_code === undefined ? {} : { error_code: v.error_code as string }), ...(steps === undefined ? {} : { steps }), ...(observed ? { observed_before: observed } : {}) }
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
  | { step: number; kind: "verify_file"; purpose: string; rule: string; relative_path: string; entry: string; expected_size: number; expected_sha256: string; prefix_from_step: number }
  | { step: number; kind: "resolve"; purpose: string; rule: string; binary: string; args: string[]; prefix_from_step: number }
export interface ProviderCliLifecycleReview {
  reviewed_at: string; reviewer: string; files: { member: string; sha256: string }[]
  scripts: Record<string, string>; runs: string[]; closure: { packages: number; with_scripts: string[] }
}
export interface ProviderCliNativeBinary { package: string; version: string; integrity: string; member: string; size: number; sha256: string; unpacked_size: number }
export interface ProviderCliRecipe extends ProviderCliRecipeIdentity {
  provider_id: string; platform: string; binary_names: string[]; supported: boolean; official_docs: string; login_hint: string
  unsupported_reason?: string; node_min?: number; lifecycle_scripts?: { enabled: boolean; review?: ProviderCliLifecycleReview }
  expected_version?: string; deadline_seconds?: number; node_arch?: string; native_binary?: ProviderCliNativeBinary
  package?: { name: string; version: string; integrity: string; registry: string }; plan?: ProviderCliPlanStep[]
}
export interface ProviderCliRecipes { schema_version: number; machine_id: string; platform: string; controller_source_sha: string; table_hash: string; recipes: ProviderCliRecipe[] }
export interface ProviderCliPrepared {
  confirmation_token?: string; expires_at?: string; machine_id: string; table_hash: string; recipe: ProviderCliRecipe
  action: ProviderCliAction; version_relation: ProviderCliRelation; target_version: string
  observation: ProviderCliObservation; observation_hash: string
}
const text = (v: unknown, max = 8192): v is string => typeof v === "string" && v.length > 0 && v.length <= max
const hash = (v: unknown): v is string => typeof v === "string" && /^[a-f0-9]{64}$/.test(v)
const slug = (v: unknown): v is string => typeof v === "string" && /^[a-z0-9][a-z0-9_-]{0,127}$/.test(v)
const strings = (v: unknown): v is string[] => Array.isArray(v) && v.length <= 64 && v.every(item => text(item))
const observationState = (v: unknown): v is ProviderCliObservation["state"] => ["absent", "installed", "unknown"].includes(v as string)
const nullableVersion = (v: unknown): v is string | null => v === null || text(v, 256)
const relativePath = (v: unknown): v is string => text(v, 512) && /^[A-Za-z0-9@._/-]+$/.test(v) && v.split("/").every(part => part !== "" && part !== "." && part !== "..")
const binaryName = (v: unknown): v is string => text(v, 128) && /^[A-Za-z0-9_-][A-Za-z0-9._-]*$/.test(v)
const semver = (v: unknown): v is string => text(v, 128) && /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(v)
const integrity = (v: unknown): v is string => text(v, 256) && /^sha512-[A-Za-z0-9+/]+={0,2}$/.test(v)
export function providerCliObservation(v: unknown): ProviderCliObservation | null {
  // Serde omits None fields in the v2 wire DTO. Normalize only those optional
  // fields; required evidence and unknown enum values still fail closed.
  if (!record(v)) return null
  v = { ...v, current_version: v.current_version ?? null, version_text: v.version_text ?? null, executable: v.executable ?? null,
    ...(record(v.probe) ? { probe: { ...v.probe, error_code: v.probe.error_code ?? null } } : {}) }
  if (!record(v) || !observationState(v.state) || !nullableVersion(v.current_version) || !nullableVersion(v.version_text) ||
    !(v.executable === null || text(v.executable) && v.executable.startsWith("/")) || typeof v.found_in_prefix !== "boolean" ||
    !record(v.probe) || !exit(v.probe.exit_code) || typeof v.probe.timed_out !== "boolean" || typeof v.probe.cleanup_confirmed !== "boolean" ||
    !(v.probe.error_code === null || code(v.probe.error_code))) return null
  if (v.state === "absent" && (v.found_in_prefix || v.current_version !== null)) return null
  if (v.state === "installed" && (!v.found_in_prefix || !v.current_version || v.probe.exit_code !== 0 || v.probe.timed_out || !v.probe.cleanup_confirmed)) return null
  if (v.node_arch !== undefined && !["arm64", "x64"].includes(v.node_arch as string)) return null
  return { ...(v.node_arch === undefined ? {} : { node_arch: v.node_arch as string }), state: v.state, current_version: v.current_version, version_text: v.version_text, executable: v.executable, found_in_prefix: v.found_in_prefix,
    probe: { exit_code: v.probe.exit_code, timed_out: v.probe.timed_out, cleanup_confirmed: v.probe.cleanup_confirmed, error_code: v.probe.error_code } }
}
export function providerCliPlan(v: unknown): ProviderCliPlanStep[] | null {
  if (!Array.isArray(v) || ![5, 9].includes(v.length)) return null
  const extended = v.length === 9
  const prefix = extended ? 7 : 4
  const purposes = extended ? ["node-arch", "pack", "pack-native", "integrity", "integrity-native", "install", "prefix", "verify-file", "check"] : ["pack", "integrity", "install", "prefix", "check"]
  const plan: ProviderCliPlanStep[] = []
  for (const [i, item] of v.entries()) {
    if (!record(item) || item.step !== i + 1 || item.purpose !== purposes[i]) return null
    const expectedKind = item.purpose.startsWith("integrity") ? "verify" : item.purpose === "verify-file" ? "verify_file" : item.purpose === "check" ? "resolve" : "command"
    if (item.kind !== expectedKind) return null
    if (item.kind === "command" && text(item.executable) && strings(item.args)) plan.push({ step: item.step, kind: item.kind, purpose: item.purpose, executable: item.executable, args: item.args })
    else if (item.kind === "verify" && item.algorithm === "sha512" && text(item.expected, 256)) plan.push({ step: item.step, kind: item.kind, purpose: item.purpose, algorithm: item.algorithm, expected: item.expected })
    else if (item.kind === "resolve" && item.prefix_from_step === prefix && text(item.rule) && binaryName(item.binary) && strings(item.args)) plan.push({ step: item.step, kind: item.kind, purpose: item.purpose, rule: item.rule, binary: item.binary, args: item.args, prefix_from_step: prefix })
    else if (item.kind === "verify_file" && item.prefix_from_step === prefix && text(item.rule) && relativePath(item.relative_path) && binaryName(item.entry) &&
      integer(item.expected_size) && item.expected_size > 0 && item.expected_size <= 1073741824 && hash(item.expected_sha256)) plan.push({ step: item.step, kind: item.kind, purpose: item.purpose, rule: item.rule, relative_path: item.relative_path, entry: item.entry, expected_size: item.expected_size, expected_sha256: item.expected_sha256, prefix_from_step: prefix })
    else return null
  }
  return plan
}
function lifecycleReview(v: unknown): ProviderCliLifecycleReview | null {
  if (!record(v) || !text(v.reviewed_at, 128) || !text(v.reviewer, 512) || !Array.isArray(v.files) || !v.files.length || v.files.length > 16 ||
    !record(v.scripts) || !Object.keys(v.scripts).length || Object.keys(v.scripts).length > 16 || !Object.entries(v.scripts).every(([key, value]) => code(key) && text(value, 4096)) ||
    !strings(v.runs) || v.runs.length !== 1 || v.runs[0] !== "postinstall" || !text(v.scripts.postinstall, 4096) ||
    !record(v.closure) || v.closure.packages !== 2 || !strings(v.closure.with_scripts) || v.closure.with_scripts.length !== 1) return null
  const files: ProviderCliLifecycleReview["files"] = []
  for (const file of v.files) {
    if (!record(file) || !relativePath(file.member) || !hash(file.sha256) || files.some(other => other.member === file.member)) return null
    files.push({ member: file.member, sha256: file.sha256 })
  }
  return { reviewed_at: v.reviewed_at, reviewer: v.reviewer, files, scripts: Object.fromEntries(Object.entries(v.scripts)) as Record<string, string>, runs: v.runs, closure: { packages: 2, with_scripts: v.closure.with_scripts } }
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
  const optional: Pick<ProviderCliRecipe, "expected_version" | "deadline_seconds" | "node_arch" | "native_binary"> = {}
  if (v.expected_version !== undefined) {
    if (!semver(v.expected_version) || v.expected_version !== v.package.version) return null
    optional.expected_version = v.expected_version
  }
  if (v.deadline_seconds !== undefined) {
    if (!integer(v.deadline_seconds) || v.deadline_seconds < 60 || v.deadline_seconds > 900) return null
    optional.deadline_seconds = v.deadline_seconds
  }
  let review: ProviderCliLifecycleReview | null = null
  if (plan.length === 9) {
    const pin = v.native_binary, file = plan[7]
    review = lifecycleReview(v.lifecycle_scripts.review)
    if (!optional.expected_version || !["arm64", "x64"].includes(v.node_arch as string) || !record(pin) || !text(pin.package, 256) || pin.version !== optional.expected_version ||
      !integrity(pin.integrity) || !relativePath(pin.member) || !nonnegative(pin.size) || pin.size === 0 || !nonnegative(pin.unpacked_size) || pin.size > pin.unpacked_size || pin.unpacked_size > 1073741824 || !hash(pin.sha256) ||
      !v.lifecycle_scripts.enabled || !review || review.closure.with_scripts[0] !== v.package.name || file.kind !== "verify_file" || file.expected_size !== pin.size || file.expected_sha256 !== pin.sha256 ||
      file.relative_path !== `lib/node_modules/${v.package.name}/bin/${file.entry}.exe` || !v.binary_names.includes(file.entry) ||
      plan[4].kind !== "verify" || plan[4].expected !== pin.integrity || plan[3].kind !== "verify" || plan[3].expected !== v.package.integrity) return null
    optional.node_arch = v.node_arch as string
    optional.native_binary = { package: pin.package, version: pin.version as string, integrity: pin.integrity, member: pin.member, size: pin.size, sha256: pin.sha256, unpacked_size: pin.unpacked_size }
  } else if (v.native_binary !== undefined || v.node_arch !== undefined || v.lifecycle_scripts.enabled) return null
  return { ...base, ...optional, node_min: v.node_min, lifecycle_scripts: { enabled: v.lifecycle_scripts.enabled, ...(review ? { review } : {}) },
    package: { name: v.package.name, version: v.package.version, integrity: v.package.integrity, registry: v.package.registry }, plan }
}
export function providerCliRecipes(v: unknown, machineId: string): ProviderCliRecipes | null {
  if (!record(v) || ![1, 2].includes(v.schema_version as number) || v.machine_id !== machineId || !text(v.platform, 128) || !text(v.controller_source_sha, 128) ||
    !hash(v.table_hash) || !Array.isArray(v.recipes) || v.recipes.length > 64) return null
  const recipes: ProviderCliRecipe[] = []
  for (const item of v.recipes) {
    const recipe = providerCliRecipe(item, v.platform)
    if (!recipe || v.schema_version === 1 && recipe.plan?.length === 9 || v.schema_version === 2 && recipe.supported && !recipe.expected_version || recipes.some(other => other.recipe_id === recipe.recipe_id)) return null
    recipes.push(recipe)
  }
  return { schema_version: v.schema_version as number, machine_id: machineId, platform: v.platform, controller_source_sha: v.controller_source_sha, table_hash: v.table_hash, recipes }
}
export function providerCliPrepared(v: unknown, recipes: ProviderCliRecipes, selected: ProviderCliRecipe): ProviderCliPrepared | null {
  if (!record(v)) return null
  v = { ...v, version_relation: v.version_relation ?? null }
  if (!record(v) || v.machine_id !== recipes.machine_id || v.table_hash !== recipes.table_hash || !hash(v.observation_hash) ||
    !["install", "replace", "noop"].includes(v.action as string) || ![null, "upgrade", "downgrade", "unknown", "incomparable"].includes(v.version_relation as null | string)) return null
  const recipe = providerCliRecipe(v.recipe, recipes.platform)
  if (!recipe?.supported || recipe.recipe_id !== selected.recipe_id || recipe.recipe_version !== selected.recipe_version || recipe.recipe_hash !== selected.recipe_hash) return null
  const observation = providerCliObservation(v.observation)
  if (!observation || v.target_version !== recipe.package!.version) return null
  if (v.action === "noop") {
    if (v.confirmation_token !== undefined || v.expires_at !== undefined || observation.state !== "installed" || observation.current_version !== v.target_version || v.version_relation !== null) return null
  } else {
    if (!text(v.confirmation_token, 512) || !text(v.expires_at, 128) || !Number.isFinite(Date.parse(v.expires_at))) return null
    if (v.action === "install" && (observation.state !== "absent" || v.version_relation !== null)) return null
    if (v.action === "replace" && (!observation.found_in_prefix || v.version_relation === null || observation.state === "absent" ||
      observation.state === "unknown" && v.version_relation !== "unknown" || observation.state === "installed" && (v.version_relation === "unknown" || observation.current_version === v.target_version))) return null
  }
  // Always show the freshly prepared plan, including its machine-resolved paths.
  return { ...(v.action === "noop" ? {} : { confirmation_token: v.confirmation_token as string, expires_at: v.expires_at as string }),
    machine_id: recipes.machine_id, table_hash: recipes.table_hash, recipe, action: v.action as ProviderCliAction,
    version_relation: v.version_relation as ProviderCliRelation, target_version: v.target_version as string, observation, observation_hash: v.observation_hash }
}
export function providerCliAvailableRecipes(catalog: ProviderCliRecipes, capabilities: readonly string[]): ProviderCliRecipes {
  return { ...catalog, recipes: catalog.recipes.filter(recipe => recipe.plan?.length !== 9 || capabilities.includes(PROVIDER_CLI_PLAN_CAPABILITY)) }
}
export const providerCliRef = (recipe: ProviderCliRecipeIdentity): ProviderCliRecipeIdentity => ({ recipe_id: recipe.recipe_id, recipe_version: recipe.recipe_version, recipe_hash: recipe.recipe_hash })
