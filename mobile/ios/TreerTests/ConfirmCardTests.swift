import XCTest
@testable import Treer

final class ConfirmCardTests: XCTestCase {
    func testActionEnumCoversRequiredMutations() {
        let raw = Set(ConfirmAction.allCases.map(\.rawValue))
        XCTAssertEqual(
            raw,
            Set(["create", "prompt", "abort", "stop", "delete", "launch", "switch_proxy", "logout"])
        )
    }

    func testSwitchProxyAndLogoutHideChange() {
        XCTAssertFalse(ConfirmAction.switchProxy.allowsChange)
        XCTAssertFalse(ConfirmAction.logout.allowsChange)
        XCTAssertTrue(ConfirmAction.create.allowsChange)
        XCTAssertTrue(ConfirmAction.prompt.allowsChange)
    }

    func testConsequenceCopy() {
        XCTAssertEqual(
            ConfirmAction.abort.consequence(),
            "Cancel the current turn. The Agent process stays running."
        )
        XCTAssertEqual(
            ConfirmAction.stop.consequence(),
            "Stop the process. You can Launch again. Transcript/PTY history follows Host retention."
        )
        XCTAssertEqual(
            ConfirmAction.delete.consequence(),
            "Remove this Agent from the workspace. The process is stopped and the workspace entry is deleted."
        )
        XCTAssertEqual(
            ConfirmAction.create.consequence(kindOrProfile: "Codex", machine: "build-machine", name: "reviewer"),
            "Start Codex on build-machine as reviewer."
        )
        XCTAssertEqual(
            ConfirmAction.prompt.consequence(machine: "build-machine", name: "reviewer"),
            "Send this follow-up to reviewer on build-machine."
        )
        XCTAssertTrue(ConfirmAction.switchProxy.consequence().contains("Leave this control plane"))
        XCTAssertTrue(ConfirmAction.logout.consequence().contains("Sign out this device"))
    }

    func testObjectIdSuffixStripsPrefixes() {
        XCTAssertEqual(ObjectIDSuffix.suffix(for: "ag_reviewerblocked"), "locked")
        XCTAssertEqual(ObjectIDSuffix.suffix(for: "srv_buildmachine"), "achine")
        XCTAssertEqual(ObjectIDSuffix.suffix(for: "short"), "short")
    }

    func testPromptPolicyConfirmsLongOrWorking() {
        XCTAssertFalse(PromptPolicy.requiresConfirmation(text: "ok", agentStatus: .idle))
        XCTAssertTrue(PromptPolicy.requiresConfirmation(text: "ok", agentStatus: .working))
        XCTAssertTrue(PromptPolicy.requiresConfirmation(text: String(repeating: "a", count: 501), agentStatus: .idle))
    }
}
