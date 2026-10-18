// Opt-in Docker smoke for the actual standalone Console hosting template.
import { execFileSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import assert from 'node:assert/strict'
const root = fileURLToPath(new URL('../', import.meta.url)).replace(/\/$/, '')
const docker = (...args) => execFileSync('docker', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim()
const id = docker('run', '--detach', '--rm', '--publish', '127.0.0.1::8080',
  '--env', 'PORT=8080', '--env', 'TREER_PROXY_PUBLIC_URL=https://proxy.example.test',
  '--env', 'NGINX_ENVSUBST_FILTER=^(PORT|TREER_PROXY_PUBLIC_URL)$',
  '--mount', `type=bind,source=${root}/dist,target=/usr/share/nginx/html,readonly`,
  '--mount', `type=bind,source=${root}/default.conf.template,target=/etc/nginx/templates/default.conf.template,readonly`,
  'nginx:1.27-alpine')
try {
  const port = docker('port', id, '8080/tcp').split(':').at(-1)
  const origin = `http://127.0.0.1:${port}`
  let ready = false
  for (let attempt = 0; attempt < 100; attempt++) {
    try { ready = (await fetch(`${origin}/health`)).ok } catch { /* startup */ }
    if (ready) break
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  assert(ready, 'nginx failed to start')
  for (const [path, method, html] of [
    ['/', 'GET', true], ['/orgs/synthetic/workspaces/test', 'GET', true],
    ['/missing.txt', 'GET', true], ['/', 'POST', true],
    ['/config.json', 'GET', false], ['/health', 'GET', false], ['/favicon.svg', 'GET', false],
  ]) {
    const response = await fetch(origin + path, { method })
    assert.equal(response.headers.get('cross-origin-opener-policy'), html ? 'same-origin' : null, `${method} ${path}`)
    assert.equal(response.headers.get('cross-origin-embedder-policy'), null)
    assert.equal(response.headers.get('x-frame-options'), 'DENY')
    assert.equal(response.headers.get('referrer-policy'), 'no-referrer')
    console.log(`${method} ${path}: ${response.status}, COOP=${response.headers.get('cross-origin-opener-policy')}`)
  }
} finally { docker('stop', id) }
