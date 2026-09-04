import SwiftUI
import UIKit
import WebKit

struct AgentAISWebView: View {
    var workspaceId: String
    var agent: AgentInfo
    var client: APIClient?
    var onClose: () -> Void
    @StateObject private var bridge = AgentUIBridge()

    var body: some View {
        NavigationStack {
            ZStack {
                if let client, agent.hasAgentUI {
                    AgentUIWebView(
                        workspaceId: workspaceId,
                        agentId: agent.agentId,
                        client: client,
                        bridge: bridge
                    )
                    .accessibilityIdentifier("agent-ui-webview")
                } else {
                    ContentUnavailableView(
                        "Agent UI unavailable",
                        systemImage: "safari",
                        description: Text("This Agent has no AIS ui_path. Stay on the native composer.")
                    )
                }
                if let error = bridge.errorMessage {
                    VStack {
                        Spacer()
                        Text(error)
                            .font(.footnote)
                            .padding(12)
                            .frame(maxWidth: .infinity)
                            .background(.ultraThinMaterial)
                    }
                }
            }
            .ignoresSafeArea(edges: .bottom)
            .navigationTitle(agent.name)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close", action: onClose)
                }
                ToolbarItem(placement: .status) {
                    StatusDot(value: agent.status.rawValue)
                }
            }
        }
    }
}

final class AgentUIBridge: NSObject, ObservableObject, WKScriptMessageHandler {
    @Published var errorMessage: String?
    weak var webView: WKWebView?
    var client: APIClient?
    var workspaceId = ""
    var agentId = ""
    private var sockets: [String: URLSessionWebSocketTask] = [:]
    private var socketSessions: [String: URLSession] = [:]

    func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage) {
        guard let body = message.body as? [String: Any] else { return }
        switch message.name {
        case "treerFetch":
            handleFetch(body)
        case "treerSocket":
            handleSocket(body)
        default:
            break
        }
    }

    private func handleFetch(_ body: [String: Any]) {
        guard let id = body["id"] as? String,
              let rawURL = body["url"] as? String,
              let client
        else { return }
        let method = (body["method"] as? String) ?? "GET"
        let headers = body["headers"] as? [String: String] ?? [:]
        let bodyData = Data(base64Encoded: (body["body"] as? String) ?? "")
        let relative = Self.relativePath(from: rawURL)
        var request = client.authorizedRequest(client.interfaceURL(workspaceId: workspaceId, agentId: agentId, relative: relative), method: method)
        for (key, value) in headers {
            if key.lowercased() == "host" || key.lowercased() == "origin" { continue }
            request.setValue(value, forHTTPHeaderField: key)
        }
        if let token = client.token {
            request.setValue("treer_session=\(token)", forHTTPHeaderField: "Cookie")
        }
        request.httpBody = bodyData
        Task {
            do {
                let (data, response) = try await URLSession.shared.data(for: request)
                let http = response as? HTTPURLResponse
                let status = http?.statusCode ?? 200
                var headerMap: [String: String] = [:]
                http?.allHeaderFields.forEach { key, value in
                    headerMap["\(key)"] = "\(value)"
                }
                let headerJSON = (try? String(data: JSONSerialization.data(withJSONObject: headerMap), encoding: .utf8)) ?? "{}"
                let b64 = data.base64EncodedString()
                let js = "window.__treerFetchResult(\(Self.jsString(id)), \(status), '', \(Self.jsString(headerJSON)), \(Self.jsString(b64)));"
                await evaluate(js)
            } catch {
                let js = "window.__treerFetchError(\(Self.jsString(id)), \(Self.jsString(error.localizedDescription)));"
                await evaluate(js)
            }
        }
    }

    private func handleSocket(_ body: [String: Any]) {
        let op = body["op"] as? String ?? ""
        guard let id = body["id"] as? String else { return }
        switch op {
        case "open":
            openSocket(id: id, rawURL: body["url"] as? String ?? "")
        case "send":
            if let text = body["text"] as? String {
                sockets[id]?.send(.string(text)) { _ in }
            }
        case "close":
            sockets[id]?.cancel(with: .normalClosure, reason: nil)
            sockets[id] = nil
            socketSessions[id]?.invalidateAndCancel()
            socketSessions[id] = nil
        default:
            break
        }
    }

    private func openSocket(id: String, rawURL: String) {
        guard let client else { return }
        let relative = Self.relativePath(from: rawURL)
        guard var request = try? {
            var req = client.authorizedRequest(client.interfaceURL(workspaceId: workspaceId, agentId: agentId, relative: relative))
            if var components = URLComponents(url: req.url!, resolvingAgainstBaseURL: false) {
                components.scheme = (components.scheme == "https") ? "wss" : "ws"
                req.url = components.url
            }
            return req
        }() else { return }
        if let token = client.token {
            request.setValue("treer_session=\(token)", forHTTPHeaderField: "Cookie")
        }
        let session = URLSession(configuration: .default)
        let task = session.webSocketTask(with: request)
        sockets[id] = task
        socketSessions[id] = session
        task.resume()
        Task { await evaluate("window.__treerSocketOpen(\(Self.jsString(id)));") }
        receive(id: id, task: task)
    }

    private func receive(id: String, task: URLSessionWebSocketTask) {
        task.receive { [weak self] result in
            guard let self else { return }
            switch result {
            case let .success(message):
                switch message {
                case let .string(text):
                    Task { await self.evaluate("window.__treerSocketMessage(\(Self.jsString(id)), \(Self.jsString(text)), false);") }
                case let .data(data):
                    Task { await self.evaluate("window.__treerSocketMessage(\(Self.jsString(id)), \(Self.jsString(data.base64EncodedString())), true);") }
                @unknown default:
                    break
                }
                self.receive(id: id, task: task)
            case .failure:
                Task { await self.evaluate("window.__treerSocketClose(\(Self.jsString(id)), 1006, '');") }
            }
        }
    }

    @MainActor
    private func evaluate(_ js: String) async {
        _ = try? await webView?.evaluateJavaScript(js)
    }

    func injectCookie(for proxy: URL, token: String) {
        guard let host = proxy.host else { return }
        let store = WKWebsiteDataStore.default().httpCookieStore
        var properties: [HTTPCookiePropertyKey: Any] = [
            .name: "treer_session",
            .value: token,
            .domain: host,
            .path: "/",
            .originURL: proxy
        ]
        if let cookie = HTTPCookie(properties: properties) {
            store.setCookie(cookie)
        }
    }

    static func relativePath(from rawURL: String) -> String {
        if let url = URL(string: rawURL) {
            let path = url.path
            let trimmed = path.hasPrefix("/") ? String(path.dropFirst()) : path
            if let query = url.query, !query.isEmpty {
                return trimmed + "?" + query
            }
            return trimmed.isEmpty ? "ws" : trimmed
        }
        return rawURL
    }

    static func jsString(_ value: String) -> String {
        let data = (try? JSONEncoder().encode(value)) ?? Data("\"\"".utf8)
        return String(data: data, encoding: .utf8) ?? "\"\""
    }
}

struct AgentUIWebView: UIViewRepresentable {
    var workspaceId: String
    var agentId: String
    var client: APIClient
    var bridge: AgentUIBridge

    func makeCoordinator() -> Coordinator {
        Coordinator(scheme: AgentUISchemeHandler())
    }

    func makeUIView(context: Context) -> WKWebView {
        bridge.client = client
        bridge.workspaceId = workspaceId
        bridge.agentId = agentId
        if let token = client.token {
            bridge.injectCookie(for: client.proxyURL, token: token)
        }

        let configuration = WKWebViewConfiguration()
        configuration.preferences.setValue(true, forKey: "allowFileAccessFromFileURLs")
        configuration.setValue(true, forKey: "allowUniversalAccessFromFileURLs")
        configuration.defaultWebpagePreferences.allowsContentJavaScript = true
        let controller = WKUserContentController()
        controller.add(bridge, name: "treerFetch")
        controller.add(bridge, name: "treerSocket")
        controller.addUserScript(WKUserScript(source: Self.interceptScript, injectionTime: .atDocumentStart, forMainFrameOnly: false))
        configuration.userContentController = controller
        context.coordinator.scheme.bundleRoot = AgentUIBundle.root
        configuration.setURLSchemeHandler(context.coordinator.scheme, forURLScheme: "treer-ui")

        let webView = WKWebView(frame: .zero, configuration: configuration)
        webView.navigationDelegate = context.coordinator
        webView.scrollView.contentInsetAdjustmentBehavior = .never
        webView.isOpaque = false
        webView.accessibilityIdentifier = "agent-ui-webview"
        bridge.webView = webView
        context.coordinator.webView = webView

        if let root = AgentUIBundle.root {
            let file = root.appendingPathComponent("index.html")
            if FileManager.default.fileExists(atPath: file.path) {
                webView.load(URLRequest(url: URL(string: "treer-ui://bundle/index.html?agent=\(agentId)")!))
            } else {
                loadFallback(on: webView)
            }
        } else {
            loadFallback(on: webView)
        }
        return webView
    }

    func updateUIView(_ webView: WKWebView, context: Context) {
        bridge.webView = webView
        bridge.client = client
        bridge.workspaceId = workspaceId
        bridge.agentId = agentId
    }

    private func loadFallback(on webView: WKWebView) {
        let url = client.interfaceURL(workspaceId: workspaceId, agentId: agentId, relative: "")
        webView.load(client.authorizedRequest(url))
    }

    static let interceptScript = """
    (function() {
      if (window.__treerNativeWrapped) return;
      window.__treerNativeWrapped = true;
      const fetchHandler = window.webkit && window.webkit.messageHandlers.treerFetch;
      const socketHandler = window.webkit && window.webkit.messageHandlers.treerSocket;
      if (!fetchHandler || !socketHandler) return;
      const pending = {};
      window.__treerFetchResult = function(id, status, statusText, headerJSON, bodyBase64) {
        const entry = pending[id];
        if (!entry) return;
        delete pending[id];
        let headers = {};
        try { headers = JSON.parse(headerJSON || '{}'); } catch (e) {}
        const bytes = Uint8Array.from(atob(bodyBase64 || ''), function(c) { return c.charCodeAt(0); });
        entry.resolve(new Response(bytes, { status: status, statusText: statusText || '', headers: headers }));
      };
      window.__treerFetchError = function(id, message) {
        const entry = pending[id];
        if (!entry) return;
        delete pending[id];
        entry.reject(new TypeError(message));
      };
      const origFetch = window.fetch.bind(window);
      window.fetch = function(input, init) {
        try {
          const req = new Request(input, init);
          const url = new URL(req.url, document.baseURI);
          const path = url.pathname || '';
          if (path.indexOf('/assets/') !== -1 || /\\.(js|css|map|woff2?|png|svg|html)$/.test(path)) {
            return origFetch(input, init);
          }
          const id = Math.random().toString(36).slice(2) + Date.now().toString(36);
          const headers = {};
          req.headers.forEach(function(v, k) { headers[k] = v; });
          return new Promise(function(resolve, reject) {
            pending[id] = { resolve: resolve, reject: reject };
            const post = function(body) {
              fetchHandler.postMessage({ id: id, url: url.toString(), method: req.method, headers: headers, body: body });
            };
            if (req.method === 'GET' || req.method === 'HEAD') {
              post(null);
            } else {
              req.arrayBuffer().then(function(buf) {
                const bytes = new Uint8Array(buf);
                let binary = '';
                for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
                post(btoa(binary));
              }).catch(reject);
            }
          });
        } catch (e) {
          return origFetch(input, init);
        }
      };
      const sockets = {};
      window.__treerSocketOpen = function(id) {
        const s = sockets[id];
        if (!s) return;
        s.readyState = 1;
        if (s.onopen) s.onopen({ type: 'open' });
      };
      window.__treerSocketMessage = function(id, data, isBinary) {
        const s = sockets[id];
        if (!s) return;
        let payload = data;
        if (isBinary) {
          const bytes = Uint8Array.from(atob(data), function(c) { return c.charCodeAt(0); });
          payload = bytes.buffer;
        }
        if (s.onmessage) s.onmessage({ data: payload });
      };
      window.__treerSocketClose = function(id, code, reason) {
        const s = sockets[id];
        if (!s) return;
        s.readyState = 3;
        if (s.onclose) s.onclose({ code: code || 1000, reason: reason || '' });
        delete sockets[id];
      };
      window.__treerSocketError = function(id, message) {
        const s = sockets[id];
        if (!s) return;
        if (s.onerror) s.onerror({ message: message });
      };
      function NativeWebSocket(url, protocols) {
        this.url = String(url);
        this.readyState = 0;
        this.bufferedAmount = 0;
        this.extensions = '';
        this.protocol = '';
        this.binaryType = 'blob';
        this.onopen = null;
        this.onclose = null;
        this.onerror = null;
        this.onmessage = null;
        this._id = Math.random().toString(36).slice(2) + Date.now().toString(36);
        sockets[this._id] = this;
        socketHandler.postMessage({ op: 'open', id: this._id, url: this.url, protocols: protocols || null });
      }
      NativeWebSocket.prototype.send = function(data) {
        socketHandler.postMessage({ op: 'send', id: this._id, text: typeof data === 'string' ? data : String(data) });
      };
      NativeWebSocket.prototype.close = function(code, reason) {
        this.readyState = 2;
        socketHandler.postMessage({ op: 'close', id: this._id, code: code || 1000, reason: reason || '' });
      };
      NativeWebSocket.prototype.addEventListener = function(type, fn) { this['on' + type] = fn; };
      NativeWebSocket.prototype.removeEventListener = function(type, fn) { if (this['on' + type] === fn) this['on' + type] = null; };
      NativeWebSocket.CONNECTING = 0;
      NativeWebSocket.OPEN = 1;
      NativeWebSocket.CLOSING = 2;
      NativeWebSocket.CLOSED = 3;
      window.WebSocket = NativeWebSocket;
    })();
    """

    final class Coordinator: NSObject, WKNavigationDelegate {
        let scheme: AgentUISchemeHandler
        weak var webView: WKWebView?

        init(scheme: AgentUISchemeHandler) {
            self.scheme = scheme
        }

        func webView(
            _ webView: WKWebView,
            decidePolicyFor navigationAction: WKNavigationAction,
            decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
        ) {
            guard let url = navigationAction.request.url else {
                decisionHandler(.allow)
                return
            }
            if url.scheme == "treer-ui" || url.scheme == "about" || url.scheme == "file" {
                decisionHandler(.allow)
                return
            }
            if navigationAction.navigationType == .linkActivated {
                UIApplication.shared.open(url)
                decisionHandler(.cancel)
                return
            }
            decisionHandler(.allow)
        }
    }
}

enum AgentUIBundle {
    static var root: URL? {
        if let url = Bundle.main.url(forResource: "index", withExtension: "html", subdirectory: "AgentUI") {
            return url.deletingLastPathComponent()
        }
        if let url = Bundle.main.resourceURL?.appendingPathComponent("AgentUI"),
           FileManager.default.fileExists(atPath: url.appendingPathComponent("index.html").path)
        {
            return url
        }
        if let url = Bundle.main.url(forResource: "index", withExtension: "html", subdirectory: "agent-ui") {
            return url.deletingLastPathComponent()
        }
        return nil
    }
}

final class AgentUISchemeHandler: NSObject, WKURLSchemeHandler {
    var bundleRoot: URL?

    func webView(_ webView: WKWebView, start urlSchemeTask: WKURLSchemeTask) {
        guard let url = urlSchemeTask.request.url else {
            urlSchemeTask.didFailWithError(URLError(.badURL))
            return
        }
        let relative = AgentUIBridge.relativePath(from: url.absoluteString)
        let fileName = relative.split(separator: "?").first.map(String.init) ?? "index.html"
        let safeName = fileName.isEmpty || fileName == "/" ? "index.html" : fileName
        guard let root = bundleRoot else {
            urlSchemeTask.didFailWithError(URLError(.fileDoesNotExist))
            return
        }
        let file = root.appendingPathComponent(safeName)
        guard FileManager.default.fileExists(atPath: file.path), let data = try? Data(contentsOf: file) else {
            urlSchemeTask.didFailWithError(URLError(.fileDoesNotExist))
            return
        }
        let mime = Self.mime(for: file.pathExtension)
        let response = URLResponse(url: url, mimeType: mime, expectedContentLength: data.count, textEncodingName: "utf-8")
        urlSchemeTask.didReceive(response)
        urlSchemeTask.didReceive(data)
        urlSchemeTask.didFinish()
    }

    func webView(_ webView: WKWebView, stop urlSchemeTask: WKURLSchemeTask) {}

    private static func mime(for ext: String) -> String {
        switch ext.lowercased() {
        case "html": return "text/html"
        case "js": return "text/javascript"
        case "css": return "text/css"
        case "json": return "application/json"
        case "svg": return "image/svg+xml"
        case "png": return "image/png"
        case "woff2": return "font/woff2"
        case "map": return "application/json"
        default: return "application/octet-stream"
        }
    }
}
