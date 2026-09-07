import Foundation
import Network
import NetworkExtension
import TreerNetworkCore

/// A UDP flow can contain multiple destinations. Each gets its own authorized
/// Controller association. Limits bound sockets and pending reply memory.
actor UDPFlowRelay {
    private let flow: NEAppProxyUDPFlow
    private let registration: CaptureRegistration
    private let resolve: @Sendable (String) -> String
    private var connections: [String: NWConnection] = [:]
    private var readers: [Task<Void, Never>] = []
    private var replies: [(Data, NWHostEndpoint)] = []
    private var writing = false
    private var stopped = false

    init(_ flow: NEAppProxyUDPFlow, registration: CaptureRegistration,
         resolve: @escaping @Sendable (String) -> String = { $0 }) {
        self.flow = flow
        self.registration = registration
        self.resolve = resolve
    }

    func run() async {
        do {
            try await flow.open(withLocalEndpoint: nil)
            while !stopped {
                let (datagrams, endpoints) = try await readBatch()
                guard let datagrams, let endpoints, datagrams.count == endpoints.count,
                      !datagrams.isEmpty else { break }
                for (data, rawEndpoint) in zip(datagrams, endpoints) {
                    let endpoint = rawEndpoint
                    guard !stopped, let port = UInt16(endpoint.port), port > 0 else {
                        throw NativeNetworkError.invalidArguments
                    }
                    let key = "\(endpoint.hostname)|\(port)"
                    let connection: NWConnection
                    if let existing = connections[key] { connection = existing }
                    else {
                        guard connections.count < 64 else { throw NativeNetworkError.unavailable }
                        connection = NWConnection(host: "127.0.0.1",
                            port: NWEndpoint.Port(rawValue: registration.socksPort)!, using: .tcp)
                        connections[key] = connection
                        try await connection.establish()
                        try await connection.socksConnect(agent: registration.agentID,
                            host: resolve(endpoint.hostname), port: port, datagram: true)
                        guard !stopped else { throw NativeNetworkError.unavailable }
                        readers.append(Task {
                            do {
                                while !Task.isCancelled {
                                    let reply = try await connection.readDatagram()
                                    try await self.deliver(reply, from: endpoint)
                                }
                            } catch { self.cancel(error) }
                        })
                    }
                    try await connection.sendDatagram(data)
                }
            }
            cancel(nil)
        } catch { cancel(error) }
    }

    private func readBatch() async throws -> ([Data]?, [NWHostEndpoint]?) {
        try await withCheckedThrowingContinuation { continuation in
            flow.readDatagrams { data, endpoints, error in
                if let error { continuation.resume(throwing: error) }
                else { continuation.resume(returning: (data, endpoints?.compactMap { $0 as? NWHostEndpoint })) }
            }
        }
    }

    private func deliver(_ data: Data, from endpoint: NWHostEndpoint) async throws {
        guard !stopped, replies.count < 64 else { throw NativeNetworkError.unavailable }
        replies.append((data, endpoint))
        guard !writing else { return }
        writing = true
        defer { writing = false }
        while !replies.isEmpty && !stopped {
            let (data, endpoint) = replies.removeFirst()
            try await flow.writeDatagrams([data], sentBy: [endpoint])
        }
    }

    func cancel(_ error: Error?) {
        guard !stopped else { return }
        stopped = true
        for connection in connections.values { connection.cancel() }
        for reader in readers { reader.cancel() }
        connections.removeAll()
        readers.removeAll()
        replies.removeAll()
        flow.closeReadWithError(error)
        flow.closeWriteWithError(error)
    }
}
