import { spawn } from 'node:child_process'
import { realpath, stat } from 'node:fs/promises'
import { resolve } from 'node:path'
import { recordBrowserPhase, finishBrowserProcess } from './controller-update-diagnostics.mjs'

// Usage: node e2e/controller-update-native.mjs CORE_OWNED_PRIVATE_ROOT
const root = await realpath(process.argv[2] ?? '')
if (process.argv.length !== 3 || !(await stat(root)).isDirectory() || ((await stat(root)).mode & 0o077)) throw Error('private fixture root required')
recordBrowserPhase(root, 'runner_starting')
const child = spawn(process.execPath, [resolve(import.meta.dirname, '../node_modules/@playwright/test/cli.js'), 'test', '--config', 'playwright.controller-update-native.config.ts'], {
  cwd: resolve(import.meta.dirname, '..'),
  env: { ...process.env, TREER_CONTROLLER_BROWSER_ROOT: root },
  stdio: 'inherit',
})
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => child.kill(signal))
child.on('error', () => { finishBrowserProcess(root); process.exitCode = 1 })
child.on('exit', code => { finishBrowserProcess(root); process.exitCode = code ?? 1 })
