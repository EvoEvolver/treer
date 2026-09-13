import Foundation
import XCTest
@testable import TreerNetworkCore

private final class MemoryStream: RelayStream {
    var onRead: ((@escaping (Result<StreamChunk, Error>) -> Void) -> Void)?
    var onWrite: ((Data, @escaping (Error?) -> Void) -> Void)?
    var onFinish: ((@escaping (Error?) -> Void) -> Void)?
    var onClose: ((Error?) -> Void)?
    func read(_ completion: @escaping (Result<StreamChunk, Error>) -> Void) { onRead?(completion) }
    func write(_ data: Data, _ completion: @escaping (Error?) -> Void) { onWrite?(data, completion) }
    func finishWrite(_ completion: @escaping (Error?) -> Void) { onFinish?(completion) }
    func close(_ error: Error?) { onClose?(error) }
}

final class TCPRelayTests: XCTestCase {
    func testRequestFINStillAllowsDelayedBinaryResponse() {
        let source = MemoryStream(), destination = MemoryStream()
        let completed = expectation(description: "both directions completed")
        let request = Data([0, 255, 1, 2]), response = Data([255, 0, 7])
        var reads = 0, destinationRead: ((Result<StreamChunk, Error>) -> Void)?
        var sourceFinished = false, destinationFinished = false
        source.onRead = { callback in
            reads += 1
            callback(.success(reads == 1 ? StreamChunk(request) : StreamChunk(end: true)))
        }
        destination.onRead = { destinationRead = $0 }
        destination.onWrite = { data, callback in XCTAssertEqual(data, request); callback(nil) }
        source.onWrite = { data, callback in
            XCTAssertTrue(destinationFinished)
            XCTAssertEqual(data, response)
            callback(nil)
        }
        destination.onFinish = { callback in
            destinationFinished = true
            callback(nil)
            // Server waits for the entire request FIN before replying. Payload
            // and EOF may arrive in the same Network.framework receive callback.
            destinationRead?(.success(StreamChunk(response, end: true)))
        }
        source.onFinish = { callback in sourceFinished = true; callback(nil) }
        for stream in [source, destination] {
            stream.onClose = { error in
                XCTAssertNil(error)
                XCTAssertTrue(sourceFinished && destinationFinished)
            }
        }
        let relay = TCPRelay(source, destination)
        relay.start { result in
            XCTAssertEqual(try? result.get(), [4, 3])
            completed.fulfill()
        }
        wait(for: [completed], timeout: 2)
    }

    func testDestinationFINDoesNotDiscardRemainingUpload() {
        // The same half-close contract must hold with the directions reversed.
        let source = MemoryStream(), destination = MemoryStream()
        let completed = expectation(description: "reverse half close")
        var sourceRead: ((Result<StreamChunk, Error>) -> Void)?
        source.onRead = { sourceRead = $0 }
        destination.onRead = { $0(.success(StreamChunk(end: true))) }
        source.onFinish = { callback in
            callback(nil)
            sourceRead?(.success(StreamChunk(Data([1, 0, 255]), end: true)))
        }
        destination.onWrite = { data, callback in XCTAssertEqual(data.count, 3); callback(nil) }
        destination.onFinish = { $0(nil) }
        TCPRelay(source, destination).start { result in
            XCTAssertEqual(try? result.get(), [3, 0]); completed.fulfill()
        }
        wait(for: [completed], timeout: 2)
    }

    func testReadWaitsForWriterAndFailureClosesBothDirections() {
        let source = MemoryStream(), destination = MemoryStream()
        let writerBlocked = expectation(description: "writer blocked")
        let completed = expectation(description: "reset")
        let state = NSLock()
        var reads = 0, closes = 0
        source.onRead = { callback in
            state.lock(); reads += 1; state.unlock()
            callback(.success(StreamChunk(Data(repeating: 255, count: 16384))))
        }
        destination.onWrite = { _, _ in writerBlocked.fulfill() }
        for stream in [source, destination] {
            stream.onClose = { error in
                XCTAssertNotNil(error)
                state.lock(); closes += 1; state.unlock()
            }
        }
        let relay = TCPRelay(source, destination)
        relay.start { result in
            if case .success = result { XCTFail("reset must fail") }
            completed.fulfill()
        }
        wait(for: [writerBlocked], timeout: 2)
        state.lock(); XCTAssertEqual(reads, 1); state.unlock()
        relay.cancel(NSError(domain: NSPOSIXErrorDomain, code: Int(ECONNRESET)))
        wait(for: [completed], timeout: 2)
        state.lock(); XCTAssertEqual(closes, 2); state.unlock()
    }
}
