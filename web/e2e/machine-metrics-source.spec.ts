// Opt-in acceptance of bytes emitted by the real Core collector/disk/query.
// This test never synthesizes metric samples, buckets, statuses or timestamps.
import { test, expect } from '@playwright/test'
import { readFileSync, statSync } from 'node:fs'
import { createHash } from 'node:crypto'
import { fixture, gotoWorkspace } from './fixtures/workspace-tabs'
const capturePath = process.env.METRICS_SOURCE_CAPTURE

test('native source query bytes render through the Machine API caller for all ranges', async ({ page }, info) => {
  test.skip(!capturePath, 'Core native query capture is required; synthetic fixtures do not satisfy this gate')
  expect(statSync(capturePath!).size).toBeLessThanOrEqual(5 * 1024 * 1024)
  const capture = JSON.parse(readFileSync(capturePath!, 'utf8')) as { source_sha: string; server_id: string; to: string; bodies: string[] }
  expect(capture.source_sha).toMatch(/^[a-f0-9]{40}$/)
  expect(capture.server_id).toMatch(/^[a-zA-Z0-9_-]{1,128}$/)
  expect(capture.bodies).toHaveLength(4)
  const end = Date.parse(capture.to)
  expect(new Date(end).toISOString()).toBe(capture.to)
  const bodies = new Map<string, string>()
  const hashes: string[] = []
  for (const body of capture.bodies) {
    expect(Buffer.byteLength(body)).toBeLessThanOrEqual(1048576)
    const response = JSON.parse(body)
    expect(response.schema).toBe('machine.metrics.v1')
    expect(response.server_id).toBe(capture.server_id)
    expect(Date.parse(response.to)).toBe(end)
    const range = (end - Date.parse(response.from)) / 60000
    expect([15, 60, 360, 1440]).toContain(range)
    expect(bodies.has(String(range))).toBe(false)
    bodies.set(String(range), body)
    hashes.push(createHash('sha256').update(body).digest('hex'))
  }
  await page.clock.setFixedTime(new Date(end))
  const live = await fixture(page)
  await page.route('**/snapshot', route => route.fulfill({ json: { ...live.snapshot(), servers: live.snapshot().servers.map(machine => ({ ...machine, server_id: capture.server_id, capabilities: ['machine.metrics.v1'] })) } }))
  const queried: number[] = []
  await page.route('**/machines/*/metrics?*', async route => {
    const url = new URL(route.request().url())
    expect(url.pathname.split('/').at(-2)).toBe(capture.server_id)
    expect(Date.parse(url.searchParams.get('to')!)).toBe(end)
    expect(url.searchParams.get('max_points')).toBe('360')
    const range = (end - Date.parse(url.searchParams.get('from')!)) / 60000
    queried.push(range)
    const body = bodies.get(String(range))
    if (body === undefined) throw new Error('Unexpected native capture range')
    // Pass the native serialized body through unchanged.
    await route.fulfill({ body, contentType: 'application/json', headers: { 'cache-control': 'no-store' } })
  })
  await gotoWorkspace(page)
  await page.getByRole('button', { name: 'Workspace settings', exact: true }).click()
  await page.getByRole('button', { name: /^workstation(\s|$)/ }).click()
  const panel = page.getByTestId('machine-metrics')
  for (const [name, range] of [['15m', 15], ['1h', 60], ['6h', 360], ['24h', 1440]] as const) {
    await panel.getByRole('button', { name, exact: true }).click()
    await expect.poll(() => queried.includes(range)).toBe(true)
    // Loading clears only after the new body passes the real decoder (or errors).
    await expect(panel.getByRole('button', { name: 'Refresh', exact: true })).toBeEnabled()
    await expect(panel.getByRole('heading', { name: 'CPU', exact: true })).toBeVisible()
    await expect(panel.getByRole('heading', { name: 'RAM', exact: true })).toBeVisible()
    await expect(panel.getByRole('alert')).toHaveCount(0)
    await expect(panel).not.toContainText('could not be verified')
  }
  expect([...new Set(queried)]).toEqual([15, 60, 360, 1440])
  await info.attach('native-source-provenance', { body: JSON.stringify({ source_sha: capture.source_sha, response_sha256: hashes, queried_ranges_minutes: queried }), contentType: 'application/json' })
})
