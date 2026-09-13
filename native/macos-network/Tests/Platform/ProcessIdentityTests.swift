import Darwin
import Foundation
import XCTest
import TreerNetworkCore

final class ProcessIdentityTests: XCTestCase {
    func testCheckpointRestoresOnlyLiveIdentitiesFromSameBoot() throws {
        let process = try XCTUnwrap(ProcessInstance.read(getpid()))
        let registration = CaptureRegistration(pid: process.pid, startSeconds: process.seconds,
            startMicroseconds: process.microseconds, agentID: "restored-agent", socksPort: 8791)
        let original = ProcessRegistry()
        XCTAssertTrue(original.register(registration))
        var token = audit_token_t()
        XCTAssertEqual(treer_test_audit_token(&token), KERN_SUCCESS)
        let bytes = withUnsafeBytes(of: token) { Data($0) }
        XCTAssertEqual(original.lookup(bytes), registration)
        let boot = try ProcessRegistry.bootSession()
        XCTAssertFalse(boot.isEmpty)
        let data = try original.checkpoint(boot: boot)
        let restored = ProcessRegistry()
        try restored.restore(data, boot: boot)
        XCTAssertEqual(restored.lookup(bytes), registration)
        let rebooted = ProcessRegistry()
        try rebooted.restore(data, boot: "another-boot")
        XCTAssertNil(rebooted.lookup(bytes))

        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        var roots = try XCTUnwrap(object["roots"] as? [[String: Any]])
        roots[0]["startSeconds"] = process.seconds + 1
        object["roots"] = roots
        object["observed"] = []
        let reused = ProcessRegistry()
        try reused.restore(JSONSerialization.data(withJSONObject: object), boot: boot)
        XCTAssertNil(reused.lookup(bytes))
        XCTAssertThrowsError(try restored.restore(Data("broken".utf8), boot: boot))
        XCTAssertEqual(restored.lookup(bytes), registration, "invalid data cannot partially replace state")
    }

    func testPrivateCheckpointStoreRoundTripAndPermissions() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let store = RegistryStore(directory: directory)
        XCTAssertNil(try store.load())
        try store.save(Data("first".utf8))
        XCTAssertEqual(try store.load(), Data("first".utf8))
        try store.save(Data("replacement".utf8))
        XCTAssertEqual(try store.load(), Data("replacement".utf8))
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: directory.path)
        XCTAssertThrowsError(try store.load())
        XCTAssertThrowsError(try store.save(Data()))
    }

    func testKernelAuditTokenResolvesRegisteredProcessInstance() throws {
        let process = try XCTUnwrap(ProcessInstance.read(getpid()))
        let registry = ProcessRegistry()
        let registration = CaptureRegistration(pid: process.pid, startSeconds: process.seconds,
            startMicroseconds: process.microseconds, agentID: "agent-test", socksPort: 8791)
        var token = audit_token_t()
        XCTAssertEqual(treer_test_audit_token(&token), KERN_SUCCESS)
        let bytes = withUnsafeBytes(of: token) { Data($0) }
        XCTAssertNil(registry.lookup(bytes))
        XCTAssertTrue(registry.register(registration))
        XCTAssertEqual(registry.lookup(bytes), registration)
        XCTAssertTrue(registry.register(registration), "registration retry is idempotent")
        var reassignment = registration
        reassignment.agentID = "different-agent"
        XCTAssertFalse(registry.register(reassignment), "live process ownership cannot be overwritten")
        XCTAssertEqual(registry.lookup(bytes), registration)
        XCTAssertNil(registry.lookup(nil))
        XCTAssertNil(registry.lookup(Data([1, 2])))
        // An audit token with another PID version is rejected by libproc even
        // though its numeric PID matches a live, registered process.
        token.val.7 ^= 0x40000000
        XCTAssertNil(registry.lookup(withUnsafeBytes(of: token) { Data($0) }))
    }

    func testRegistrationRejectsWrongBirthTimeAndUnsupportedProtocol() throws {
        let process = try XCTUnwrap(ProcessInstance.read(getpid()))
        var registration = CaptureRegistration(pid: process.pid, startSeconds: process.seconds + 1,
            startMicroseconds: process.microseconds, agentID: "agent-test", socksPort: 8791)
        let registry = ProcessRegistry()
        XCTAssertFalse(registry.register(registration))
        registration.startSeconds = process.seconds
        registration.version = 2
        XCTAssertFalse(registry.register(registration))
        registration.version = 1
        registration.agentID = String(repeating: "x", count: 256)
        XCTAssertFalse(registry.register(registration))
        registration.agentID = "agent\nforged"
        XCTAssertFalse(registry.register(registration))
        registration.agentID = "agent-test"
        registration.socksPort = 0
        XCTAssertFalse(registry.register(registration))
    }

    func testReparentingDoesNotChangeProcessBirthIdentity() {
        let original = ProcessInstance(pid: 123, parent: 100, seconds: 10, microseconds: 50)
        let detached = ProcessInstance(pid: 123, parent: 1, seconds: 10, microseconds: 50)
        let reused = ProcessInstance(pid: 123, parent: 1, seconds: 11, microseconds: 50)
        XCTAssertTrue(original.sameBirth(as: detached))
        XCTAssertFalse(original.sameBirth(as: reused))
    }
}
