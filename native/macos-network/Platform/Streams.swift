import Foundation
import NetworkExtension
import TreerNetworkCore

final class FlowStream: RelayStream {
    let flow: NEAppProxyTCPFlow
    init(_ flow: NEAppProxyTCPFlow) { self.flow = flow }
    func read(_ completion: @escaping (Result<StreamChunk, Error>) -> Void) {
        flow.readData { data, error in
            if let error { completion(.failure(error)) }
            else { completion(.success(StreamChunk(data ?? Data(), end: data?.isEmpty != false))) }
        }
    }
    func write(_ data: Data, _ completion: @escaping (Error?) -> Void) {
        flow.write(data, withCompletionHandler: completion)
    }
    func finishWrite(_ completion: @escaping (Error?) -> Void) {
        flow.closeWriteWithError(nil)
        completion(nil)
    }
    func close(_ error: Error?) {
        flow.closeReadWithError(error)
        flow.closeWriteWithError(error)
    }
}
