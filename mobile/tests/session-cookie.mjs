// Foundation-only regression; no simulator, device, account or real token.
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs'
import { spawnSync } from 'node:child_process'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
const dir = mkdtempSync(join(tmpdir(), 'treer-cookie-test-'))
const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => /^(PATH|TMPDIR|LANG|LC_ALL)$/.test(key)))
env.HOME = dir
const source = `import Foundation
let https = URL(string: "https://proxy.example.test/")!
let token = "synthetic-session"
let legacy = HTTPCookie(properties: [.name: "treer_session", .value: token, .domain: "proxy.example.test", .path: "/", .originURL: https])!
assert(!legacy.isHTTPOnly && !legacy.isSecure)
let protected = SessionCookie.make(for: https, token: token)!
assert(protected.isHTTPOnly && protected.isSecure && protected.path == "/")
assert(protected.domain == "proxy.example.test")
assert(SessionCookie.header(for: https, token: token)!.contains("SameSite=Strict"))
assert(SessionCookie.make(for: URL(string: "http://127.0.0.1/")!, token: token)!.isHTTPOnly)
assert(!SessionCookie.make(for: URL(string: "http://127.0.0.1/")!, token: token)!.isSecure)
assert(SessionCookie.make(for: https, token: "fake; Domain=example.test") == nil)
assert(SessionCookie.make(for: URL(string: "https://user:fake@proxy.example.test/")!, token: token) == nil)
assert(SessionCookie.make(for: URL(string: "javascript:alert(1)")!, token: token) == nil)
print("Foundation session-cookie checks passed (synthetic values only)")
`
try {
  writeFileSync(join(dir, 'main.swift'), source, { mode: 0o600 })
  for (const [command, args] of [
    ['swiftc', ['-module-cache-path', join(dir, 'cache'), resolve('mobile/ios/Treer/Core/SessionCookie.swift'), join(dir, 'main.swift'), '-o', join(dir, 'check')]],
    [join(dir, 'check'), []],
  ]) {
    const result = spawnSync(command, args, { env, stdio: 'inherit' })
    if (result.error) throw result.error
    if (result.status !== 0) { process.exitCode = result.status ?? 1; break }
  }
} finally { rmSync(dir, { recursive: true, force: true }) }
