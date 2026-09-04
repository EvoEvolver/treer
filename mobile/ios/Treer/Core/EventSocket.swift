import Foundation

final class EventSocket {
    private var task: URLSessionWebSocketTask?
    private var session: URLSession?
    private var receiveLoop: Task<Void, Never>?

    var onSnapshot: ((WorkspaceSnapshot) -> Void)?
    var onState: ((ConnectionState) -> Void)?
    var onUnauthorized: (() -> Void)?

    func connect(request: URLRequest) {
        disconnect()
        onState?(.connecting)
        let session = URLSession(configuration: .default)
        self.session = session
        let task = session.webSocketTask(with: request)
        self.task = task
        task.resume()
        onState?(.live)
        receiveLoop = Task { [weak self] in
            await self?.receiveForever()
        }
    }

    func disconnect() {
        receiveLoop?.cancel()
        receiveLoop = nil
        task?.cancel(with: .goingAway, reason: nil)
        task = nil
        session?.invalidateAndCancel()
        session = nil
    }

    private func receiveForever() async {
        while !Task.isCancelled {
            guard let task else { return }
            do {
                let message = try await task.receive()
                switch message {
                case let .string(text):
                    handle(text)
                case let .data(data):
                    if let text = String(data: data, encoding: .utf8) {
                        handle(text)
                    }
                @unknown default:
                    break
                }
            } catch {
                if Task.isCancelled { return }
                onState?(.reconnecting)
                try? await Task.sleep(nanoseconds: 1_200_000_000)
                onState?(.offline)
                return
            }
        }
    }

    private func handle(_ text: String) {
        guard let data = text.data(using: .utf8) else { return }
        if let event = try? TreerJSON.decoder.decode(WorkspaceEvent.self, from: data),
           event.event == "workspace.snapshot",
           let snapshot = event.data
        {
            onSnapshot?(snapshot)
            return
        }
        if let snapshot = try? TreerJSON.decoder.decode(WorkspaceSnapshot.self, from: data) {
            onSnapshot?(snapshot)
        }
    }
}
