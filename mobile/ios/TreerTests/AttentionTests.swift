import XCTest
@testable import Treer

final class AttentionTests: XCTestCase {
    func testBlockedAndFailedAreAttention() {
        XCTAssertTrue(Attention.needsAttention(agent: FixtureData.blockedAgent, machine: FixtureData.onlineMachine))
        XCTAssertTrue(Attention.needsAttention(agent: FixtureData.failedAgent, machine: FixtureData.onlineMachine))
    }

    func testLongWorkingOnOnlineMachineIsNotAttention() {
        XCTAssertFalse(Attention.needsAttention(agent: FixtureData.workingAgent, machine: FixtureData.onlineMachine))
    }

    func testIdleOnOnlineMachineIsNotAttention() {
        XCTAssertFalse(Attention.needsAttention(agent: FixtureData.idleAgent, machine: FixtureData.onlineMachine))
    }

    func testNonTerminalOnOfflineMachineIsAttention() {
        XCTAssertTrue(Attention.needsAttention(agent: FixtureData.offlineHungAgent, machine: FixtureData.offlineMachine))
        let idleOffline = AgentInfo(
            agentId: "ag_idleoff",
            serverId: FixtureData.offlineMachine.serverId,
            kind: "codex",
            name: "parked",
            status: .idle
        )
        XCTAssertTrue(Attention.needsAttention(agent: idleOffline, machine: FixtureData.offlineMachine))
    }

    func testExitedOnOfflineMachineIsNotAttention() {
        let exited = AgentInfo(
            agentId: "ag_exitedoff",
            serverId: FixtureData.offlineMachine.serverId,
            kind: "codex",
            name: "done",
            status: .exited
        )
        XCTAssertFalse(Attention.needsAttention(agent: exited, machine: FixtureData.offlineMachine))
    }

    func testHomeTruncatesOverflowIntoInboxCount() {
        var agents: [AgentInfo] = []
        for index in 0 ..< 11 {
            agents.append(
                AgentInfo(
                    agentId: "ag_block\(index)",
                    serverId: FixtureData.onlineMachine.serverId,
                    kind: "codex",
                    name: "blocked-\(index)",
                    status: .blocked,
                    updatedAt: Date().addingTimeInterval(TimeInterval(-index))
                )
            )
        }
        let snapshot = WorkspaceSnapshot(
            revision: 1,
            workspace: FixtureData.workspace,
            servers: [FixtureData.onlineMachine],
            agents: agents
        )
        let home = Attention.homeAttention(in: snapshot)
        XCTAssertEqual(home.items.count, 8)
        XCTAssertEqual(home.overflow, 3)
        XCTAssertEqual(Attention.items(in: snapshot).count, 11)
    }

    func testUnknownIsIdleStyleNotInboxUnlessOffline() {
        XCTAssertFalse(Attention.needsAttention(agent: FixtureData.unknownAgent, machine: FixtureData.onlineMachine))
        XCTAssertTrue(Attention.idle(in: FixtureData.snapshot).contains(where: { $0.agentId == FixtureData.unknownAgent.agentId }))
        XCTAssertEqual(FixtureData.unknownAgent.displayStatus(machine: FixtureData.offlineMachine), "offline")
    }

    func testFixtureInboxContainsBlockedFailedAndOfflineHung() {
        let ids = Set(Attention.items(in: FixtureData.snapshot).map(\.agentId))
        XCTAssertTrue(ids.contains(FixtureData.blockedAgent.agentId))
        XCTAssertTrue(ids.contains(FixtureData.failedAgent.agentId))
        XCTAssertTrue(ids.contains(FixtureData.offlineHungAgent.agentId))
        XCTAssertFalse(ids.contains(FixtureData.workingAgent.agentId))
        XCTAssertFalse(ids.contains(FixtureData.idleAgent.agentId))
    }
}
