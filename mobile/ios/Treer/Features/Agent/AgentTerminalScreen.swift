import SwiftUI

struct AgentTerminalScreen: View {
    var workspaceId: String
    var agent: AgentInfo
    var client: APIClient?
    var onClose: () -> Void
    @StateObject private var session = TerminalSession()

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                ScrollViewReader { proxy in
                    ScrollView {
                        Text(session.output.isEmpty ? "Connecting…" : session.output)
                            .font(.system(.footnote, design: .monospaced))
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(12)
                            .id("tail")
                    }
                    .background(Color.black)
                    .foregroundStyle(Color.green)
                    .onChange(of: session.output) { _, _ in
                        proxy.scrollTo("tail", anchor: .bottom)
                    }
                }
                keyBar
            }
            .background(Color.black.ignoresSafeArea())
            .navigationTitle(agent.name)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close", action: onClose)
                }
            }
            .task {
                await session.connect(workspaceId: workspaceId, agentId: agent.agentId, client: client)
            }
            .onDisappear { session.disconnect() }
        }
        .accessibilityIdentifier("agent-terminal")
    }

    private var keyBar: some View {
        VStack(spacing: 8) {
            HStack(spacing: 6) {
                key("Esc") { session.send("\u{1b}") }
                key("Tab") { session.send("\t") }
                key("Ctrl-C") { session.send("\u{03}") }
                key("Enter") { session.send("\r") }
            }
            HStack(spacing: 6) {
                key("←") { session.send("\u{1b}[D") }
                key("↑") { session.send("\u{1b}[A") }
                key("↓") { session.send("\u{1b}[B") }
                key("→") { session.send("\u{1b}[C") }
            }
        }
        .padding(10)
        .background(Color(.secondarySystemBackground))
    }

    private func key(_ title: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(title)
                .font(.caption.weight(.semibold))
                .frame(maxWidth: .infinity, minHeight: 40)
        }
        .buttonStyle(.bordered)
    }
}

@MainActor
final class TerminalSession: ObservableObject {
    @Published var output = ""
    private var task: URLSessionWebSocketTask?
    private var urlSession: URLSession?
    private var sessionId = "term"
    private let maxCharacters = 50_000

    func connect(workspaceId: String, agentId: String, client: APIClient?) async {
        guard let client else {
            output = "No session."
            return
        }
        guard let request = try? client.terminalWebSocketRequest(workspaceId: workspaceId, agentId: agentId, cols: 80, rows: 24) else {
            output = "Invalid terminal URL."
            return
        }
        let session = URLSession(configuration: .default)
        urlSession = session
        let task = session.webSocketTask(with: request)
        self.task = task
        task.resume()
        receive(task)
    }

    func disconnect() {
        task?.cancel(with: .goingAway, reason: nil)
        task = nil
        urlSession?.invalidateAndCancel()
        urlSession = nil
    }

    func send(_ text: String) {
        guard let task else { return }
        let frame = TerminalBinaryFrame(kind: .input, sessionId: sessionId, revision: 0, payload: Data(text.utf8))
        guard let encoded = try? frame.encode() else { return }
        task.send(.data(encoded)) { _ in }
    }

    private func receive(_ task: URLSessionWebSocketTask) {
        task.receive { [weak self] result in
            Task { @MainActor in
                guard let self else { return }
                switch result {
                case let .success(message):
                    switch message {
                    case let .data(data):
                        self.handle(data)
                    case let .string(text):
                        self.append(text)
                    @unknown default:
                        break
                    }
                    self.receive(task)
                case let .failure(error):
                    self.append("\n[disconnected] \(error.localizedDescription)\n")
                }
            }
        }
    }

    private func handle(_ data: Data) {
        guard let frame = try? TerminalBinaryFrame.decode(data) else {
            if let text = String(data: data, encoding: .utf8) {
                append(text)
            }
            return
        }
        sessionId = frame.sessionId
        if frame.kind == .output || frame.kind == .ready {
            if let text = String(data: frame.payload, encoding: .utf8), !text.isEmpty {
                append(ANSIStripper.strip(text))
            }
        }
    }

    private func append(_ text: String) {
        output += text
        if output.count > maxCharacters {
            output = String(output.suffix(maxCharacters))
        }
    }
}
