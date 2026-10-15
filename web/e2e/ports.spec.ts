import { test, expect } from '@playwright/test'
import { testPorts } from './ports'

test('test server ports derive from PW_PORT without overlapping', () => {
  expect(testPorts('18371')).toEqual({ web: 18371, worker: 18372 })
  expect(testPorts('65534')).toEqual({ web: 65534, worker: 65535 })
  for (const value of ['', '0', '-1', '65535', 'NaN', '4000.5', '4000;other']) {
    expect(() => testPorts(value)).toThrow('PW_PORT must be an integer')
  }
})

test('an occupied Worker port fails startup instead of testing an unrelated HTTP server', async ({}, testInfo) => {
  test.setTimeout(45_000)
  const { createServer } = await import('node:http')
  const { execFile } = await import('node:child_process')
  const { promisify } = await import('node:util')
  const { createServer: createProbe } = await import('node:net')
  // OS-assigned port; the fake service deliberately answers /health with 200.
  const server = createServer((_request, response) => response.end('unrelated development server'))
  let workerPort = 0
  for (let attempt = 0; attempt < 10; attempt++) {
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
    const candidate = (server.address() as import('node:net').AddressInfo).port
    const probe = createProbe()
    try {
      await new Promise<void>((resolve, reject) => {
        probe.once('error', reject)
        probe.listen(candidate - 1, '127.0.0.1', resolve)
      })
      await new Promise<void>((resolve, reject) => probe.close((error) => error ? reject(error) : resolve()))
      workerPort = candidate
      break
    } catch {
      await new Promise<void>((resolve) => server.close(() => resolve()))
    }
  }
  expect(workerPort, 'could not allocate a free adjacent test port').toBeGreaterThan(0)
  try {
    let failure: { code?: number; stdout?: string; stderr?: string } | undefined
    try {
      await promisify(execFile)(process.execPath, ['e2e/run.mjs', 'security.spec.ts', '--grep', '^CSP restricts', '--workers=1', '--output', testInfo.outputPath('nested-worker-results')], {
        env: { ...process.env, PW_PORT: String(workerPort - 1) }, timeout: 30_000,
      })
    } catch (error) { failure = error as typeof failure }
    expect(failure?.code).toBe(1)
    const output = `${failure?.stdout ?? ''}\n${failure?.stderr ?? ''}`
    expect(output).toContain(`http://127.0.0.1:${workerPort}/health is already used`)
    expect(output).not.toContain('Running 1 test')
  } finally {
    await new Promise<void>((resolve, reject) => server.close((error) => error ? reject(error) : resolve()))
  }
})
