import Foundation
import Network
import XCTest
@testable import TreerNetworkCore

final class NetworkStreamTests: XCTestCase {
    func testSOCKSIdentityDestinationAndPolicyDenial() async throws {
        for datagram in [false, true] {
        let server = try NWListener(using: .tcp, on: .any)
        let ready = expectation(description: "SOCKS listening")
        let handled = expectation(description: "SOCKS policy denial")
        defer { server.cancel() }
        server.stateUpdateHandler = { if case .ready = $0 { ready.fulfill() } }
        server.newConnectionHandler = { connection in
            Task {
                defer { connection.cancel(); handled.fulfill() }
                do {
                    try await connection.establish()
                    let greeting = try await connection.readExactly(3)
                    XCTAssertEqual(greeting, Data([5, 1, 2]))
                    // Split server responses to exercise exact-read framing.
                    try await connection.sendBytes(Data([5]))
                    try await connection.sendBytes(Data([2]))
                    let auth = try await connection.readExactly(2)
                    XCTAssertEqual(auth, Data([1, 10]))
                    let username = try await connection.readExactly(10)
                    XCTAssertEqual(String(data: username, encoding: .utf8), "agent-test")
                    let password = try await connection.readExactly(6)
                    XCTAssertEqual(password, Data([5]) + Data("treer".utf8))
                    try await connection.sendBytes(Data([1, 0]))
                    let header = try await connection.readExactly(5)
                    XCTAssertEqual(header, Data([5, datagram ? 0xf0 : 1, 0, 3, 16]))
                    let destination = try await connection.readExactly(18)
                    XCTAssertEqual(destination, Data("db.treer.invalid".utf8) + Data([1, 187]))
                    try await connection.sendBytes(Data([5, 2, 0, 1, 0, 0, 0, 0, 0, 0]))
                } catch { XCTFail("SOCKS server: \(error)") }
            }
        }
        server.start(queue: .global())
        await fulfillment(of: [ready], timeout: 5)
        let client = NWConnection(host: "127.0.0.1", port: try XCTUnwrap(server.port), using: .tcp)
        defer { client.cancel() }
        try await client.establish()
        do {
            try await client.socksConnect(agent: "agent-test", host: "db.treer.invalid", port: 443, datagram: datagram)
            XCTFail("Policy denial must not open a data stream")
        } catch NativeNetworkError.rejected {
            // Expected: no direct fallback follows a denial.
        } catch { XCTFail("Unexpected SOCKS error: \(error)") }
        await fulfillment(of: [handled], timeout: 5)
        }
    }

    func testDatagramFramingPreservesEmptyAndLargePayloads() async throws {
        let server = try NWListener(using: .tcp, on: .any)
        let ready = expectation(description: "datagram bridge listening")
        let handled = expectation(description: "datagrams echoed")
        defer { server.cancel() }
        server.stateUpdateHandler = { if case .ready = $0 { ready.fulfill() } }
        let packets = [Data(), Data([0, 255, 1]), Data(repeating: 42, count: 65507)]
        server.newConnectionHandler = { connection in
            Task {
                defer { connection.cancel(); handled.fulfill() }
                do {
                    try await connection.establish()
                    for expected in packets {
                        let packet = try await connection.readDatagram()
                        XCTAssertEqual(packet, expected)
                        try await connection.sendDatagram(packet)
                    }
                } catch { XCTFail("datagram server: \(error)") }
            }
        }
        server.start(queue: .global())
        await fulfillment(of: [ready], timeout: 5)
        let client = NWConnection(host: "127.0.0.1", port: try XCTUnwrap(server.port), using: .tcp)
        defer { client.cancel() }
        try await client.establish()
        for packet in packets {
            try await client.sendDatagram(packet)
            let reply = try await client.readDatagram()
            XCTAssertEqual(reply, packet)
        }
        do {
            try await client.sendDatagram(Data(repeating: 0, count: 65508))
            XCTFail("oversized datagram accepted")
        } catch NativeNetworkError.invalidArguments { }
        await fulfillment(of: [handled], timeout: 5)
    }

    func testRealTCPFINAfterLargeRequestPreservesDelayedReply() async throws {
        let server = try NWListener(using: .tcp, on: .any)
        let bridge = try NWListener(using: .tcp, on: .any)
        let serverReady = expectation(description: "server listening")
        let bridgeReady = expectation(description: "bridge listening")
        let serverFinished = expectation(description: "server waited for FIN")
        let relayFinished = expectation(description: "relay drained both directions")
        let payload = Data((0..<(1024 * 1024)).map { UInt8($0 & 255) })
        let response = Data([71, 85, 69, 83, 84, 58, 0, 255])
        defer { bridge.cancel(); server.cancel() }
        server.stateUpdateHandler = { if case .ready = $0 { serverReady.fulfill() } }
        bridge.stateUpdateHandler = { if case .ready = $0 { bridgeReady.fulfill() } }
        server.newConnectionHandler = { connection in
            Task {
                do {
                    try await connection.establish()
                    let stream = ConnectionStream(connection)
                    var received = Data()
                    while true {
                        let chunk = try await read(stream)
                        received.append(chunk.data)
                        if chunk.end { break }
                    }
                    XCTAssertEqual(received, payload)
                    try await connection.sendBytes(response)
                    try await finish(stream)
                    serverFinished.fulfill()
                } catch { XCTFail("server: \(error)"); serverFinished.fulfill() }
            }
        }
        server.start(queue: .global())
        await fulfillment(of: [serverReady], timeout: 5)
        let serverPort = try XCTUnwrap(server.port)
        bridge.newConnectionHandler = { inbound in
            Task {
                do {
                    let outbound = NWConnection(host: "127.0.0.1", port: serverPort, using: .tcp)
                    try await inbound.establish()
                    try await outbound.establish()
                    TCPRelay(ConnectionStream(inbound), ConnectionStream(outbound)).start { result in
                        XCTAssertEqual(try? result.get(), [UInt64(payload.count), UInt64(response.count)])
                        relayFinished.fulfill()
                    }
                } catch { XCTFail("bridge: \(error)"); relayFinished.fulfill() }
            }
        }
        bridge.start(queue: .global())
        await fulfillment(of: [bridgeReady], timeout: 5)
        let client = NWConnection(host: "127.0.0.1", port: try XCTUnwrap(bridge.port), using: .tcp)
        defer { client.cancel() }
        try await client.establish()
        try await client.sendBytes(payload)
        let stream = ConnectionStream(client)
        try await finish(stream)
        var received = Data()
        while true {
            let chunk = try await read(stream)
            received.append(chunk.data)
            if chunk.end { break }
        }
        XCTAssertEqual(received, response)
        await fulfillment(of: [serverFinished, relayFinished], timeout: 5)
    }
}

private func read(_ stream: RelayStream) async throws -> StreamChunk {
    try await withCheckedThrowingContinuation { continuation in stream.read { continuation.resume(with: $0) } }
}

private func finish(_ stream: RelayStream) async throws {
    try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
        stream.finishWrite { error in
            if let error { continuation.resume(throwing: error) }
            else { continuation.resume() }
        }
    }
}
