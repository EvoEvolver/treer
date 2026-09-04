import XCTest
@testable import Treer

final class CreateFlowTests: XCTestCase {
    func testTerminalWiresToCommand() {
        XCTAssertEqual(AgentCatalog.wireKind("terminal"), "command")
        XCTAssertEqual(AgentCatalog.wireKind("codex"), "codex")
        XCTAssertEqual(AgentCatalog.wireKind("cursor-agent"), "cursor")
    }

    func testPrefersCodexWhenTheMachineReportsIt() {
        XCTAssertEqual(AgentCatalog.preferredKind(on: FixtureData.onlineMachine), "codex")
        XCTAssertEqual(AgentCatalog.preferredKind(on: FixtureData.offlineMachine), "terminal")
        XCTAssertEqual(AgentCatalog.preferredKind(on: nil), "terminal")
    }

    func testBootstrapFixtureHasBothCommands() {
        XCTAssertTrue(BootstrapInfo.fixture.installCommand.contains("install.sh"))
        XCTAssertTrue(BootstrapInfo.fixture.connectCommand.contains("TREER_ENROLLMENT_KEY"))
    }

    func testTokenStoreFallsBackWhenKeychainEntitlementIsMissing() throws {
        final class MissingEntitlementStore: TokenStore {
            func readToken(account: String) throws -> String? {
                throw TokenStoreError.unexpectedStatus(errSecMissingEntitlement)
            }
            func writeToken(_ token: String, account: String) throws {
                throw TokenStoreError.unexpectedStatus(errSecMissingEntitlement)
            }
            func deleteToken(account: String) throws {
                throw TokenStoreError.unexpectedStatus(errSecMissingEntitlement)
            }
        }
        let defaults = UserDefaults(suiteName: "treer-token-test")!
        defaults.removePersistentDomain(forName: "treer-token-test")
        let store = FallbackTokenStore(primary: MissingEntitlementStore(), defaults: defaults)
        try store.writeToken("abc", account: "session")
        XCTAssertEqual(try store.readToken(account: "session"), "abc")
        try store.deleteToken(account: "session")
        XCTAssertNil(try store.readToken(account: "session"))
    }
}
