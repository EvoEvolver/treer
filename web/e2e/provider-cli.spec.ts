import { test, expect } from '@playwright/test'

test('provider installer binds poll results and never infers successful installation', async ({ page }) => {
  await page.route('**/api/**', route => route.fulfill({ status: 401, json: { error: { message: 'Sign in' } } }))
  await page.goto('/')
  const result = await page.evaluate(async () => {
    // @ts-expect-error Vite serves this browser module.
    const { providerCliOperation: parse, providerCliTerminal: terminal, matchingProviderRecipe: match, providerCliBase } = await import('/src/lib/provider-cli.ts')
    const expected = { operation_id: 'op-test', machine_id: 'srv-test', recipe_id: 'codex', recipe_version: 1, recipe_hash: 'a'.repeat(64) }
    const valid = { ...expected, status: 'succeeded', step: 5, elapsed_ms: 500, exit_code: 0, timed_out: false, stdout: '<script>alert(1)</script>', stderr: '', truncated: false }
    const invalid = [
      { machine_id: 'other' }, { operation_id: 'other' }, { recipe_id: 'other' }, { recipe_version: 2 }, { recipe_hash: 'b'.repeat(64) },
      { status: 'invented' }, { status: 'running' }, { step: 6 }, { exit_code: 1 }, { timed_out: true }, { elapsed_ms: -1 },
      { stdout: 'x'.repeat(32769) }, { error_code: 'Bearer secret' }, { steps: [{ step: 1, exit_code: 0, stdout: '', stderr: '', truncated: false, duration_ms: -1 }] },
    ]
    const recipe = { supported: true, binary_names: ['codex'], recipe_id: 'codex' }
    return {
      valid: parse({ ...valid, confirmation_token: 'must-not-survive' }, expected),
      invalid: invalid.map(patch => parse({ ...valid, ...patch }, expected)),
      terminal: ['queued', 'running', 'succeeded', 'failed', 'timed_out', 'unknown'].map(terminal),
      matches: [
        match([recipe], 'codex', 'identity_removed', 'srv-test', 'srv-test'),
        match([recipe], '/usr/bin/codex', 'identity_removed', 'srv-test', 'srv-test'),
        match([recipe], 'codex', 'reserved_outcome_unknown', 'srv-test', 'srv-test'),
        match([recipe], 'codex', 'cleanup_failed', 'srv-test', 'srv-test'),
        match([recipe], 'codex', 'identity_removed', 'other', 'srv-test'),
        match([recipe], 'codex --help', 'identity_removed', 'srv-test', 'srv-test'),
        match([recipe, recipe], 'codex', 'identity_removed', 'srv-test', 'srv-test'),
        match([{ ...recipe, supported: false }], 'codex', 'identity_removed', 'srv-test', 'srv-test'),
      ],
      base: providerCliBase('ws/one', 'srv/two'),
    }
  })
  expect(result.valid?.stdout).toBe('<script>alert(1)</script>')
  expect(result.valid).not.toHaveProperty('confirmation_token')
  expect(result.invalid).toEqual(Array(14).fill(null))
  expect(result.terminal).toEqual([false, false, true, true, true, true])
  expect(result.matches[0]?.recipe_id).toBe('codex')
  expect(result.matches.slice(1)).toEqual(Array(7).fill(null))
  expect(result.base).toBe('/api/workspaces/ws%2Fone/machines/srv%2Ftwo/provider-clis')
})

test('provider consent validates fresh observation summaries and pre-step failures', async ({ page }) => {
  await page.route('**/api/**', route => route.fulfill({ status: 401, json: { error: { message: 'Sign in' } } }))
  await page.goto('/')
  const result = await page.evaluate(async () => {
    // @ts-expect-error Vite browser module.
    const { providerCliOperation: parse, providerCliObservation: observation } = await import('/src/lib/provider-cli.ts')
    const expected = { operation_id: 'op-test', machine_id: 'srv-test', recipe_id: 'codex', recipe_version: 1, recipe_hash: 'a'.repeat(64) }
    const summary = { state: 'installed', current_version: '2.0.0', found_in_prefix: true, observation_hash: 'b'.repeat(64), executable: '/private/never-persist' }
    const failure = { ...expected, status: 'failed', step: 0, elapsed_ms: 100, exit_code: null, timed_out: false, stdout: '', stderr: '', truncated: false, error_code: 'provider_cli_state_changed', steps: [], observed_before: summary }
    const invalid = [
      { error_code: 'other_failure' }, { status: 'running' },
      { steps: [{ step: 1, exit_code: 0, stdout: '', stderr: '', truncated: false, duration_ms: 1 }] },
      { observed_before: { ...summary, observation_hash: 'bad' } },
      { observed_before: { ...summary, state: 'invented' } },
      { observed_before: { ...summary, current_version: null } },
      { observed_before: { ...summary, found_in_prefix: false } },
      { observed_before: { ...summary, state: 'absent' } },
    ]
    const installed = { ...summary, version_text: 'codex 2.0.0', probe: { exit_code: 0, timed_out: false, cleanup_confirmed: true, error_code: null } }
    return { valid: parse(failure, expected), invalid: invalid.map(patch => parse({ ...failure, ...patch }, expected)),
      observations: [installed, { ...installed, probe: { ...installed.probe, cleanup_confirmed: false } }, { ...installed, executable: 'relative' }, { ...installed, version_text: 'x'.repeat(257) }].map(observation) }
  })
  expect(result.valid?.step).toBe(0)
  expect(result.valid?.observed_before).not.toHaveProperty('executable')
  expect(result.invalid).toEqual(Array(8).fill(null))
  expect(result.observations[0]?.current_version).toBe('2.0.0')
  expect(result.observations.slice(1)).toEqual(Array(3).fill(null))
})
