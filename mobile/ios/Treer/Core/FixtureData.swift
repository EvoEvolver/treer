import Foundation

enum FixtureData {
    static let organization = Organization(organizationId: "org_fixture", name: "Lab", role: "owner")
    static let workspace = Workspace(workspaceId: "ws_fixture", name: "workshop", createdAt: Date())

    static let onlineMachine = ServerInfo(
        serverId: "srv_buildmachine",
        workspaceId: workspace.workspaceId,
        name: "build-machine",
        hostname: "build-machine.local",
        root: "/Users/lab/treer",
        controllerBuild: BuildInfo(version: "0.1.0", gitCommit: "abc12345"),
        hostBuild: BuildInfo(version: "0.1.0", gitCommit: "abc12345"),
        supervision: MachineSupervision(mode: "launchd", fallbackReason: nil),
        availableAgents: ["codex", "claude"],
        status: .online
    )

    static let offlineMachine = ServerInfo(
        serverId: "srv_gpuoffline",
        workspaceId: workspace.workspaceId,
        name: "gpu-box",
        hostname: "gpu.local",
        root: "/opt/treer",
        status: .offline
    )

    static let blockedAgent = AgentInfo(
        agentId: "ag_reviewerblocked",
        workspaceId: workspace.workspaceId,
        serverId: onlineMachine.serverId,
        kind: "codex",
        name: "reviewer",
        status: .blocked,
        updatedAt: Date().addingTimeInterval(-40),
        interface: AgentInterface(capabilities: ["prompt.submit", "transcript.read", "abort"], uiPath: "/ui")
    )

    static let workingAgent = AgentInfo(
        agentId: "ag_trainerworking",
        workspaceId: workspace.workspaceId,
        serverId: onlineMachine.serverId,
        kind: "codex",
        name: "trainer",
        status: .working,
        updatedAt: Date().addingTimeInterval(-12),
        outputRevision: 4,
        interface: AgentInterface(capabilities: ["prompt.submit", "transcript.read", "ui_path"], uiPath: "/ui")
    )

    static let idleAgent = AgentInfo(
        agentId: "ag_shellidle0001",
        workspaceId: workspace.workspaceId,
        serverId: onlineMachine.serverId,
        kind: "command",
        name: "shell",
        status: .idle,
        updatedAt: Date().addingTimeInterval(-3600)
    )

    static let offlineHungAgent = AgentInfo(
        agentId: "ag_hungoffline01",
        workspaceId: workspace.workspaceId,
        serverId: offlineMachine.serverId,
        kind: "claude",
        name: "nightly",
        status: .working,
        updatedAt: Date().addingTimeInterval(-600)
    )

    static let failedAgent = AgentInfo(
        agentId: "ag_failedagent01",
        workspaceId: workspace.workspaceId,
        serverId: onlineMachine.serverId,
        kind: "pi",
        name: "summarizer",
        status: .failed,
        updatedAt: Date().addingTimeInterval(-90)
    )

    static let unknownAgent = AgentInfo(
        agentId: "ag_unknownagent1",
        workspaceId: workspace.workspaceId,
        serverId: onlineMachine.serverId,
        kind: "codex",
        name: "mystery",
        status: .unknown,
        updatedAt: Date().addingTimeInterval(-20)
    )

    static let snapshot = WorkspaceSnapshot(
        revision: 12,
        workspace: workspace,
        servers: [onlineMachine, offlineMachine],
        agents: [blockedAgent, workingAgent, idleAgent, offlineHungAgent, failedAgent, unknownAgent]
    )

    static let profile = LaunchProfile(
        profileId: "lp_codex",
        workspaceId: workspace.workspaceId,
        name: "Codex",
        command: "codex",
        args: ["--dangerously-bypass-approvals-and-sandbox"]
    )
}
