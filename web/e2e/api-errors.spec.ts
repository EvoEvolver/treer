import { test, expect } from '@playwright/test'

test('API retains structured errors without interpreting missing-command messages', async ({ page }) => {
  await page.route('**/api/**', route => route.fulfill({ status: 401, json: { error: { message: 'Sign in' } } }))
  await page.goto('/')
  await page.route('**/api/test-error', route => route.fulfill({ status: 422, json: {
    error: { code: 'command_not_found', message: 'Command is unavailable', details: {
      command: 'claude', machine_id: 'srv-test', agent_id: 'ag-test', creation_state: 'identity_removed',
    } },
  } }))
  const structured = await page.evaluate(async () => {
    // Vite serves this browser module; no Node-side window shim or real API.
    // @ts-expect-error Browser module URL, resolved by Vite.
    const { api, ApiError } = await import('/src/lib/api.ts')
    try { await api('/api/test-error') } catch (error) {
      if (!(error instanceof ApiError)) throw error
      const result = error as Error & { status: number; code?: string; details?: unknown }
      return { status: result.status, message: result.message, code: result.code, details: result.details }
    }
  })
  expect(structured).toEqual({ status: 422, message: 'Command is unavailable', code: 'command_not_found', details: {
    command: 'claude', machine_id: 'srv-test', agent_id: 'ag-test', creation_state: 'identity_removed',
  } })

  await page.route('**/api/test-error', route => route.fulfill({ status: 400, json: { error: { message: 'command_not_found: claude' } } }))
  const legacy = await page.evaluate(async () => {
    // @ts-expect-error Browser module URL, resolved by Vite.
    const { api, ApiError } = await import('/src/lib/api.ts')
    try { await api('/api/test-error') } catch (error) {
      if (!(error instanceof ApiError)) throw error
      const result = error as Error & { status: number; code?: string; details?: unknown }
      return { status: result.status, message: result.message, code: result.code ?? null, details: result.details ?? null }
    }
  })
  expect(legacy).toEqual({ status: 400, message: 'command_not_found: claude', code: null, details: null })
})
