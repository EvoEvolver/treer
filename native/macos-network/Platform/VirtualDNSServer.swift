import Foundation
import Network
import TreerNetworkCore

/// Loopback-only DNS, with UDP and TCP on the same ephemeral port. The provider
/// owns exact-domain resolver files pointing here, so no global resolver or
/// Tailscale configuration is replaced.
actor VirtualDNSServer {
    private var udp: NWListener?
    private var tcp: NWListener?
    private var connections: [UUID: NWConnection] = [:]
    private var stopped = false
    private let answer: @Sendable (Data) throws -> Data
    init(answer: @escaping @Sendable (Data) throws -> Data) { self.answer = answer }

    func start() async throws -> UInt16 {
        let parameters = NWParameters.udp
        parameters.requiredLocalEndpoint = .hostPort(host: "127.0.0.1", port: .any)
        let listener = try NWListener(using: parameters)
        udp = listener
        listener.newConnectionHandler = { connection in Task { await self.serve(connection, datagram: true) } }
        try await ready(listener)
        guard let port = listener.port else { throw NativeNetworkError.unavailable }
        let streamParameters = NWParameters.tcp
        streamParameters.requiredLocalEndpoint = .hostPort(host: "127.0.0.1", port: port)
        let streamListener = try NWListener(using: streamParameters)
        tcp = streamListener
        streamListener.newConnectionHandler = { connection in Task { await self.serve(connection, datagram: false) } }
        try await ready(streamListener)
        return port.rawValue
    }

    private func ready(_ listener: NWListener) async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            let gate = ListenerReady(listener, continuation)
            listener.stateUpdateHandler = { state in
                switch state {
                case .ready: gate.finish(nil)
                case .failed(let error): gate.finish(error)
                case .cancelled: gate.finish(NativeNetworkError.unavailable)
                default: break
                }
            }
            let queue = DispatchQueue(label: "treer.virtual-dns.listen")
            listener.start(queue: queue)
            queue.asyncAfter(deadline: .now() + 5) { gate.finish(NativeNetworkError.timeout) }
        }
    }

    private func serve(_ connection: NWConnection, datagram: Bool) async {
        guard !stopped, connections.count < 256 else { connection.cancel(); return }
        let id = UUID()
        connections[id] = connection
        // Bound lifetime even when a peer sends only a partial TCP question.
        let timeout = Task {
            try? await Task.sleep(nanoseconds: 10_000_000_000)
            if !Task.isCancelled { connection.cancel() }
        }
        defer { timeout.cancel(); connection.cancel(); connections.removeValue(forKey: id) }
        do {
            try await connection.establish()
            let question: Data
            if datagram {
                question = try await withCheckedThrowingContinuation { continuation in
                    connection.receiveMessage { data, _, _, error in
                        if let error { continuation.resume(throwing: error) }
                        else if let data { continuation.resume(returning: data) }
                        else { continuation.resume(throwing: NativeNetworkError.malformedReply) }
                    }
                }
            } else { question = try await connection.readDatagram() }
            guard question.count <= 4096 else { throw NativeNetworkError.invalidArguments }
            let response = try answer(question)
            if datagram { try await connection.sendBytes(response) }
            else { try await connection.sendDatagram(response) }
        } catch { /* malformed or closed DNS client; no external fallback */ }
    }

    func stop() {
        stopped = true
        udp?.cancel(); tcp?.cancel()
        udp = nil; tcp = nil
        for connection in connections.values { connection.cancel() }
        connections.removeAll()
    }
}

private final class ListenerReady {
    private let listener: NWListener
    private var continuation: CheckedContinuation<Void, Error>?
    init(_ listener: NWListener, _ continuation: CheckedContinuation<Void, Error>) {
        self.listener = listener; self.continuation = continuation
    }
    func finish(_ error: Error?) {
        guard let callback = continuation else { return }
        continuation = nil
        listener.stateUpdateHandler = nil
        if let error { listener.cancel(); callback.resume(throwing: error) }
        else { callback.resume() }
    }
}
