// Compile the real Foundation networking client with only UIDevice stubbed.
// URLProtocol intercepts every request; no simulator, real server or credentials.
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs'
import { spawnSync } from 'node:child_process'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
const dir = mkdtempSync(join(tmpdir(), 'treer-native-auth-test-'))
const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => /^(PATH|TMPDIR|LANG|LC_ALL)$/.test(key)))
env.HOME = dir
const stub = 'public struct UIDevice { public static let current = UIDevice(); public var name: String { "synthetic-device" } }'
const test = `import Foundation
final class FakeProtocol: URLProtocol {
    private static let lock = NSLock()
    private static var recorded: [URLRequest] = []
    static func requests() -> [URLRequest] { lock.lock(); defer { lock.unlock() }; return recorded }
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        Self.lock.lock(); Self.recorded.append(request); Self.lock.unlock()
        let body = request.url!.path == "/api/auth/me" ? #"{"user_id":"synthetic-user","email":"synthetic@example.test","preferred_name":"Synthetic"}"# : #"{"ok":true}"#
        let response = HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: nil, headerFields: ["Content-Type":"application/json", "Set-Cookie":"treer_session=synthetic-cookie; Path=/"])!
        client!.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client!.urlProtocol(self, didLoad: Data(body.utf8))
        client!.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}
@main struct Check {
    static func main() async throws {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [FakeProtocol.self]
        let session = URLSession(configuration: config)
        defer { session.invalidateAndCancel() }
        let api = APIClient(proxyURL: URL(string: "https://synthetic.example.test")!, token: "synthetic-token", session: session)
        try await api.requestPasswordReset(email: "synthetic@example.test")
        try await api.resetPassword(token: "synthetic-reset", password: "synthetic-password")
        _ = try await api.me()
        let requests = FakeProtocol.requests()
        assert(requests.count == 3)
        assert(requests[0].url!.path == "/api/auth/request-password-reset")
        assert(requests[1].url!.path == "/api/auth/reset-password")
        for request in requests.prefix(2) {
            assert(request.httpMethod == "POST")
            assert(request.value(forHTTPHeaderField: "X-Treer-Client") == "mobile_ios")
            assert(request.value(forHTTPHeaderField: "Authorization") == nil)
            assert(request.value(forHTTPHeaderField: "Origin") == nil)
            assert(request.value(forHTTPHeaderField: "Cookie") == nil)
            assert(!request.httpShouldHandleCookies)
        }
        assert(requests[2].value(forHTTPHeaderField: "X-Treer-Client") == nil)
        assert(requests[2].value(forHTTPHeaderField: "Authorization") == "Bearer synthetic-token")
        print("Native pre-auth request checks passed (URLProtocol only)")
    }
}
`
try {
  writeFileSync(join(dir, 'UIKit.swift'), stub, { mode: 0o600 })
  writeFileSync(join(dir, 'Check.swift'), test, { mode: 0o600 })
  const core = resolve('mobile/ios/Treer/Core')
  for (const [command, args] of [
    ['swiftc', ['-module-cache-path', join(dir, 'cache'), '-parse-as-library', '-emit-module', '-emit-object', '-module-name', 'UIKit', join(dir, 'UIKit.swift'), '-emit-module-path', join(dir, 'UIKit.swiftmodule'), '-o', join(dir, 'UIKit.o')]],
    ['swiftc', ['-module-cache-path', join(dir, 'cache'), '-parse-as-library', '-I', dir, join(core, 'APIClient.swift'), join(core, 'Models.swift'), join(core, 'JSONCoding.swift'), join(dir, 'UIKit.o'), join(dir, 'Check.swift'), '-o', join(dir, 'check')]],
    [join(dir, 'check'), []],
  ]) {
    const result = spawnSync(command, args, { env, stdio: 'inherit' })
    if (result.error) throw result.error
    if (result.status !== 0) { process.exitCode = result.status ?? 1; break }
  }
} finally { rmSync(dir, { recursive: true, force: true }) }
