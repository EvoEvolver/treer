import Foundation
import XCTest
@testable import TreerNetworkCore

final class VirtualDNSTests: XCTestCase {
    private func query(_ name: String, type: UInt8 = 1) -> Data {
        var data = Data([0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0])
        for label in name.split(separator: ".") {
            data.append(UInt8(label.utf8.count)); data.append(Data(label.utf8))
        }
        data.append(contentsOf: [0, 0, type, 0, 1])
        return data
    }

    func testSharedNamesStableAcrossControllersRemovalAndRestore() throws {
        var dns = VirtualDNS()
        try dns.replace(controller: 8001, hosts: ["API.Treer.Invalid."])
        let answer = try dns.answer(query("api.treer.invalid"))
        XCTAssertEqual(Array(answer.suffix(4)), [198, 18, 0, 1])
        try dns.replace(controller: 9001, hosts: ["other.treer.invalid", "api.treer.invalid"])
        try dns.replace(controller: 8001, hosts: [])
        XCTAssertEqual(try dns.answer(query("api.treer.invalid")), answer)
        let restored = try JSONDecoder().decode(VirtualDNS.self, from: JSONEncoder().encode(dns))
        XCTAssertEqual(restored.reverse["198.18.0.1"], "api.treer.invalid")
        XCTAssertEqual(restored.reverse[VirtualDNS.canonicalAddress("::ffff:c612:1")], "api.treer.invalid")
        XCTAssertEqual(try restored.answer(query("api.treer.invalid")), answer)
        try dns.replace(controller: 9001, hosts: [])
        XCTAssertEqual(try dns.answer(query("api.treer.invalid"))[3], 3)
        try dns.replace(controller: 8001, hosts: ["new.treer.invalid"])
        XCTAssertEqual(dns.reverse["198.18.0.1"], "api.treer.invalid", "retired address cannot change identity")
    }

    func testAAAAUnknownNamesAndMalformedPackets() throws {
        var dns = VirtualDNS()
        try dns.replace(controller: 8001, hosts: ["api.treer.invalid"])
        let aaaa = try dns.answer(query("api.treer.invalid", type: 28))
        XCTAssertEqual(aaaa[3], 0)
        XCTAssertEqual(aaaa[7], 0)
        XCTAssertEqual(try dns.answer(query("unknown.treer.invalid"))[3], 3)
        XCTAssertThrowsError(try dns.replace(controller: 8001, hosts: ["../../etc/passwd"]))
        XCTAssertThrowsError(try dns.answer(Data()))
        var cycle = query("api.treer.invalid")
        cycle[12] = 0xc0; cycle[13] = 12
        XCTAssertThrowsError(try dns.answer(cycle))
        var truncated = query("api.treer.invalid"); truncated.removeLast()
        XCTAssertThrowsError(try dns.answer(truncated))
    }
}
