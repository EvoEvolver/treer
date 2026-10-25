import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, readFileSync, rmSync, statSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import SafeBrowserReporter, { phases, recordBrowserPhase, finishBrowserProcess } from './controller-update-diagnostics.mjs'
function fixture(fn) {
  const root = mkdtempSync(join(tmpdir(), 'treer222-diagnostic-'))
  try { fn(root) } finally { rmSync(root, { recursive: true, force: true }) }
}
test('failure exposes phase only, no error/title/URL/attachments', () => fixture(root => {
  const reporter = new SafeBrowserReporter(); reporter.root = root
  reporter.onTestBegin(); recordBrowserPhase(root, 'normal_login')
  reporter.onTestEnd({ title: 'secret' }, { status: 'failed', error: { message: 'password secret' }, attachments: [{ body: 'cookie' }] })
  assert.deepEqual(JSON.parse(readFileSync(join(root, 'browser-diagnostic.json'))), { schema_version: 1, phase: 'normal_login', status: 'failed' })
  assert.equal(statSync(join(root, 'browser-diagnostic.json')).mode & 0o777, 0o600)
}))
test('browser setup runner error is distinguished and enum-only', () => fixture(root => {
  recordBrowserPhase(root, 'runner_starting')
  const reporter = new SafeBrowserReporter(); reporter.root = root
  reporter.onTestBegin(); reporter.onError({ message: 'private browser launch path' }); reporter.onTestEnd({}, { status: 'failed' })
  assert.deepEqual(JSON.parse(readFileSync(join(root, 'browser-diagnostic.json'))), { schema_version: 1, phase: 'browser_setup', status: 'runner_error' })
}))
test('process exit without final reporter result never passes', () => fixture(root => {
  recordBrowserPhase(root, 'runner_starting'); finishBrowserProcess(root)
  assert.equal(JSON.parse(readFileSync(join(root, 'browser-diagnostic.json'))).status, 'runner_error')
  recordBrowserPhase(root, 'writing_result')
  const reporter = new SafeBrowserReporter(); reporter.root = root
  reporter.onTestEnd({}, { status: 'passed' }); finishBrowserProcess(root)
  assert.equal(JSON.parse(readFileSync(join(root, 'browser-diagnostic.json'))).status, 'passed')
}))
test('unknown phase rejected without modifying receipt', () => fixture(root => {
  for (const phase of phases) recordBrowserPhase(root, phase)
  const before = readFileSync(join(root, 'browser-diagnostic.json'), 'utf8')
  assert.throws(() => recordBrowserPhase(root, 'private URL'))
  assert.equal(readFileSync(join(root, 'browser-diagnostic.json'), 'utf8'), before)
}))
