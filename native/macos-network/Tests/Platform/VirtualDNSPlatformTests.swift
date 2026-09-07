import Foundation
import Network
import XCTest
import TreerNetworkCore

final class VirtualDNSPlatformTests: XCTestCase {
    func testResolverFilesRestorePortRemoveOwnedAndPreserveForeignChanges() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let writer = ResolverFiles(directory: directory)
        try writer.replace(hosts: ["api.treer.invalid"], port: 12345)
        let file = try XCTUnwrap(FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil).first)
        XCTAssertTrue(try String(contentsOf: file, encoding: .utf8).contains("port 12345"))
        let restarted = ResolverFiles(directory: directory)
        try restarted.replace(hosts: ["api.treer.invalid"], port: 54321)
        XCTAssertTrue(try String(contentsOf: file, encoding: .utf8).contains("port 54321"))
        try restarted.replace(hosts: [], port: 54321)
        XCTAssertFalse(FileManager.default.fileExists(atPath: file.path))
        try writer.replace(hosts: ["api.treer.invalid"], port: 12345)
        try Data("user resolver\n".utf8).write(to: file)
        XCTAssertThrowsError(try writer.replace(hosts: ["api.treer.invalid"], port: 12345))
        try writer.replace(hosts: [], port: 12345)
        XCTAssertEqual(try String(contentsOf: file, encoding: .utf8), "user resolver\n")
        XCTAssertThrowsError(try writer.replace(hosts: ["../../passwd"], port: 12345))
    }

    func testLoopbackDNSAnswersOverUDPAndTCP() async throws {
        var map = VirtualDNS()
        try map.replace(controller: 8791, hosts: ["api.treer.invalid"])
        let dns = map
        let server = VirtualDNSServer { try dns.answer($0) }
        let port = try await server.start()
        let query = Data([0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0,
                          3, 97, 112, 105, 5, 116, 114, 101, 101, 114,
                          7, 105, 110, 118, 97, 108, 105, 100, 0, 0, 1, 0, 1])
        let udp = NWConnection(host: "127.0.0.1", port: NWEndpoint.Port(rawValue: port)!, using: .udp)
        let tcp = NWConnection(host: "127.0.0.1", port: NWEndpoint.Port(rawValue: port)!, using: .tcp)
        do {
            try await udp.establish()
            try await udp.sendBytes(query)
            let reply: Data = try await withCheckedThrowingContinuation { continuation in
                udp.receiveMessage { data, _, _, error in
                    if let error { continuation.resume(throwing: error) }
                    else if let data { continuation.resume(returning: data) }
                    else { continuation.resume(throwing: NativeNetworkError.unavailable) }
                }
            }
            XCTAssertEqual(reply, try dns.answer(query))
            try await tcp.establish()
            try await tcp.sendDatagram(query)
            let streamReply = try await tcp.readDatagram()
            XCTAssertEqual(streamReply, reply)
        } catch {
            udp.cancel(); tcp.cancel(); await server.stop()
            throw error
        }
        udp.cancel(); tcp.cancel(); await server.stop()
    }
}
