import Foundation

// Keep native WebView session cookies as protected as Proxy-issued cookies.
// A cookie does not confine the native bridge's request authority.
enum SessionCookie {
    static func header(for proxy: URL, token: String) -> String? {
        guard ["https", "http"].contains(proxy.scheme ?? ""), proxy.host != nil,
              proxy.user == nil, proxy.password == nil,
              !token.isEmpty,
              token.range(of: "[^A-Za-z0-9_.~-]", options: .regularExpression) == nil
        else { return nil }
        return "treer_session=\(token); Path=/; HttpOnly; SameSite=Strict" +
            (proxy.scheme == "https" ? "; Secure" : "")
    }

    static func make(for proxy: URL, token: String) -> HTTPCookie? {
        guard let header = header(for: proxy, token: token) else { return nil }
        return HTTPCookie.cookies(withResponseHeaderFields: ["Set-Cookie": header], for: proxy).first
    }
}
