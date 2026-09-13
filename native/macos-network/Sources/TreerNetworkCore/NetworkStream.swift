import Foundation
import Network

public final class ConnectionStream: RelayStream {
    let connection: NWConnection
    public init(_ connection: NWConnection) { self.connection = connection }
    public func read(_ completion: @escaping (Result<StreamChunk, Error>) -> Void) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 16384) { data, _, end, error in
            if let error { completion(.failure(error)) }
            else { completion(.success(StreamChunk(data ?? Data(), end: end))) }
        }
    }
    public func write(_ data: Data, _ completion: @escaping (Error?) -> Void) {
        connection.send(content: data, completion: .contentProcessed { completion($0) })
    }
    public func finishWrite(_ completion: @escaping (Error?) -> Void) {
        connection.send(content: nil, contentContext: .finalMessage, isComplete: true,
                        completion: .contentProcessed { completion($0) })
    }
    public func close(_ error: Error?) { connection.cancel() }
}

public enum NativeNetworkError: Error { case timeout, malformedReply, rejected, unavailable, invalidArguments }

public extension NWConnection {
    func establish() async throws {
        let queue = DispatchQueue(label: "treer.network.connect")
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            let gate = ConnectionReady(self, continuation)
            stateUpdateHandler = { state in
                switch state {
                case .ready: gate.complete(nil)
                case .failed(let error), .waiting(let error): gate.complete(error)
                case .cancelled: gate.complete(NativeNetworkError.unavailable)
                default: break
                }
            }
            start(queue: queue)
            queue.asyncAfter(deadline: .now() + 10) { gate.complete(NativeNetworkError.timeout) }
        }
    }

    func sendBytes(_ data: Data) async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            send(content: data, completion: .contentProcessed { error in
                if let error { continuation.resume(throwing: error) }
                else { continuation.resume() }
            })
        }
    }

    func readExactly(_ count: Int) async throws -> Data {
        if count == 0 { return Data() }
        return try await withCheckedThrowingContinuation { continuation in
            receive(minimumIncompleteLength: count, maximumLength: count) { data, _, _, error in
                if let error { continuation.resume(throwing: error) }
                else if let data, data.count == count { continuation.resume(returning: data) }
                else { continuation.resume(throwing: NativeNetworkError.malformedReply) }
            }
        }
    }

    func socksConnect(agent: String, host: String, port: UInt16, datagram: Bool = false) async throws {
        let username = Data(agent.utf8), destination = Data(host.utf8)
        guard !username.isEmpty, username.count <= 255, !destination.isEmpty,
              destination.count <= 255, port > 0 else { throw NativeNetworkError.invalidArguments }
        try await sendBytes(Data([5, 1, 2]))
        guard try await readExactly(2) == Data([5, 2]) else { throw NativeNetworkError.rejected }
        try await sendBytes(Data([1, UInt8(username.count)]) + username + Data([5]) + Data("treer".utf8))
        guard try await readExactly(2) == Data([1, 0]) else { throw NativeNetworkError.rejected }
        try await sendBytes(Data([5, datagram ? 0xf0 : 1, 0, 3, UInt8(destination.count)]) + destination
                            + Data([UInt8(port >> 8), UInt8(port & 255)]))
        let reply = try await readExactly(4)
        guard reply[0] == 5, reply[1] == 0, reply[2] == 0 else { throw NativeNetworkError.rejected }
        switch reply[3] {
        case 1: _ = try await readExactly(6)
        case 4: _ = try await readExactly(18)
        case 3:
            let length = try await readExactly(1)
            _ = try await readExactly(Int(length[0]) + 2)
        default: throw NativeNetworkError.malformedReply
        }
    }

    func sendDatagram(_ data: Data) async throws {
        guard data.count <= 65507 else { throw NativeNetworkError.invalidArguments }
        try await sendBytes(Data([UInt8(data.count >> 8), UInt8(data.count & 255)]) + data)
    }

    func readDatagram() async throws -> Data {
        let header = try await readExactly(2)
        let count = Int(header[0]) * 256 + Int(header[1])
        guard count <= 65507 else { throw NativeNetworkError.malformedReply }
        return try await readExactly(count)
    }
}

private final class ConnectionReady: @unchecked Sendable {
    private let connection: NWConnection
    private var continuation: CheckedContinuation<Void, Error>?
    // All calls run on NWConnection's serial queue, including its deadline.
    init(_ connection: NWConnection, _ continuation: CheckedContinuation<Void, Error>) {
        self.connection = connection
        self.continuation = continuation
    }
    func complete(_ error: Error?) {
        guard let callback = continuation else { return }
        continuation = nil
        connection.stateUpdateHandler = nil
        if let error { connection.cancel(); callback.resume(throwing: error) }
        else { callback.resume() }
    }
}
