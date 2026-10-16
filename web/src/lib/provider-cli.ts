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
    typeof v.status !== "string" || !statuses.includes(v.status) || (v.step !== null && !step(v.step)) ||
    !nonnegative(v.elapsed_ms) || !exit(v.exit_code) || typeof v.timed_out !== "boolean" ||
    !tail(v.stdout) || !tail(v.stderr) || typeof v.truncated !== "boolean" || (v.error_code !== undefined && !code(v.error_code))) return null
  if ((v.status === "queued" || v.status === "running") && (v.stdout !== "" || v.stderr !== "" || v.steps !== undefined)) return null
  if (v.status === "succeeded" && (v.exit_code !== 0 || v.timed_out || v.step !== 5)) return null
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
  return { ...expected, status: v.status as ProviderCliStatus, step: v.step as number | null, elapsed_ms: v.elapsed_ms,
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
