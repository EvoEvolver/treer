import Foundation

public struct StreamChunk {
    public let data: Data
    public let end: Bool
    public init(_ data: Data = Data(), end: Bool = false) {
        self.data = data
        self.end = end
    }
}

/// Each operation completes exactly once. close must unblock outstanding reads
/// and writes. A successful finishWrite sends FIN without closing the read side.
public protocol RelayStream: AnyObject {
    func read(_ completion: @escaping (Result<StreamChunk, Error>) -> Void)
    func write(_ data: Data, _ completion: @escaping (Error?) -> Void)
    func finishWrite(_ completion: @escaping (Error?) -> Void)
    func close(_ error: Error?)
}

/// One outstanding read/write per direction provides transport backpressure.
/// FIN is directional: drain payload, finish the destination writer, then wait
/// for the opposite direction. Only two completed directions or an error close
/// the underlying transports. All state changes occur on the private queue.
public final class TCPRelay {
    private let streams: [RelayStream]
    private let queue = DispatchQueue(label: "treer.network.tcp-relay")
    private var ended = [false, false]
    private var bytes: [UInt64] = [0, 0]
    private var stopped = false
    private var completion: ((Result<[UInt64], Error>) -> Void)?

    public init(_ source: RelayStream, _ destination: RelayStream) {
        streams = [source, destination]
    }

    public func start(_ completion: @escaping (Result<[UInt64], Error>) -> Void) {
        queue.async {
            guard self.completion == nil, !self.stopped else { return }
            self.completion = completion
            self.copy(0)
            self.copy(1)
        }
    }

    public func cancel(_ error: Error) {
        queue.async { self.stop(error) }
    }

    private func copy(_ direction: Int) {
        guard !stopped else { return }
        streams[direction].read { result in
            self.queue.async {
                guard !self.stopped else { return }
                switch result {
                case .failure(let error): self.stop(error)
                case .success(let chunk):
                    if chunk.data.isEmpty {
                        if chunk.end { self.finish(direction) }
                        else { self.stop(RelayError.emptyRead) }
                        return
                    }
                    self.streams[1 - direction].write(chunk.data) { error in
                        self.queue.async {
                            guard !self.stopped else { return }
                            if let error { self.stop(error); return }
                            self.bytes[direction] += UInt64(chunk.data.count)
                            if chunk.end { self.finish(direction) }
                            else { self.copy(direction) }
                        }
                    }
                }
            }
        }
    }

    private func finish(_ direction: Int) {
        streams[1 - direction].finishWrite { error in
            self.queue.async {
                guard !self.stopped else { return }
                if let error { self.stop(error); return }
                self.ended[direction] = true
                if self.ended.allSatisfy({ $0 }) { self.stop(nil) }
            }
        }
    }

    private func stop(_ error: Error?) {
        guard !stopped else { return }
        stopped = true
        streams.forEach { $0.close(error) }
        let callback = completion
        completion = nil
        if let error { callback?(.failure(error)) }
        else { callback?(.success(bytes)) }
    }
}

public enum RelayError: Error { case emptyRead }
