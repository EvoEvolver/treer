import SwiftUI

struct AgentDetailScreen: View {
    @EnvironmentObject private var session: AppSession
    var agentId: String
    @State private var draft = ""
    @State private var transcript: String?
    @State private var showUI = false
    @State private var showTerminal = false
    @State private var sending = false

    var body: some View {
        let agent = session.snapshot?.agents.first(where: { $0.agentId == agentId })
        let machine = agent.flatMap { session.snapshot?.machine(for: $0) }
        ScrollView {
            if let agent {
                VStack(alignment: .leading, spacing: 16) {
                    header(agent, machine)
                    capabilities(agent)
                    latestTurn(agent)
                    composer(agent, machine)
                    actions(agent, machine)
                }
                .padding(16)
            } else {
                ContentUnavailableView("Agent not in snapshot", systemImage: "questionmark.circle")
                    .padding()
            }
        }
        .navigationTitle(agent?.name ?? "Agent")
        .navigationBarTitleDisplayMode(.inline)
        .accessibilityIdentifier("agent-detail")
        .task { transcript = await session.loadTranscript(agentId: agentId) }
        .refreshable {
            await session.refreshSnapshot()
            transcript = await session.loadTranscript(agentId: agentId)
        }
        .fullScreenCover(isPresented: $showUI) {
            if let agent, let workspace = session.selectedWorkspace {
                AgentAISWebView(
                    workspaceId: workspace.workspaceId,
                    agent: agent,
                    client: session.clientForTunnel()
                ) {
                    showUI = false
                }
            }
        }
        .fullScreenCover(isPresented: $showTerminal) {
            if let agent, let workspace = session.selectedWorkspace {
                AgentTerminalScreen(
                    workspaceId: workspace.workspaceId,
                    agent: agent,
                    client: session.clientForTunnel()
                ) {
                    showTerminal = false
                }
            }
        }
    }

    private func header(_ agent: AgentInfo, _ machine: ServerInfo?) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                StatusDot(value: agent.displayStatus(machine: machine), warningUnknown: true)
                Spacer()
                Text(agent.kind).font(.caption).foregroundStyle(.secondary)
            }
            labeled("Agent ID", agent.agentId)
            labeled("Machine", "\(machine?.displayName ?? agent.serverId) · \(machine?.isOnline == true ? "online" : "offline")")
            Text("updated \(agent.updatedAt, style: .relative) · output revision \(agent.outputRevision)")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    private func capabilities(_ agent: AgentInfo) -> some View {
        let badges: [(String, Bool)] = [
            ("prompt.submit", agent.interface?.supports("prompt.submit") == true),
            ("transcript.read", agent.interface?.supports("transcript.read") == true),
            ("state.observe", agent.interface?.supports("state.observe") == true),
            ("abort", agent.canAbort),
            ("ui_path", agent.hasAgentUI)
        ]
        return VStack(alignment: .leading, spacing: 8) {
            Text("AIS")
                .font(.headline)
            FlowBadges(items: badges)
            if agent.interface == nil {
                Text("This Agent has no Agent Interface. Use the composer fallback or the emergency terminal.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private func latestTurn(_ agent: AgentInfo) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Latest turn")
                .font(.headline)
            if let transcript, !transcript.isEmpty {
                Text(transcript)
                    .font(.body)
                    .textSelection(.enabled)
            } else if agent.canReadTranscript {
                Text("Waiting for the Agent to become ready.")
                    .foregroundStyle(.secondary)
            } else {
                Text("output revision \(agent.outputRevision) · updated \(agent.updatedAt, style: .relative)")
                    .foregroundStyle(.secondary)
            }
        }
    }

    private func composer(_ agent: AgentInfo, _ machine: ServerInfo?) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Follow-up")
                .font(.headline)
            TextField("Message the Agent", text: $draft, axis: .vertical)
                .lineLimit(3 ... 8)
                .textFieldStyle(.roundedBorder)
                .disabled(machine?.isOnline != true || session.isOffline)
                .accessibilityIdentifier("follow-up-field")
            Button {
                sendFollowUp(agent)
            } label: {
                HStack {
                    Text("Send")
                    if sending { ProgressView() }
                }
            }
            .buttonStyle(.borderedProminent)
            .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || machine?.isOnline != true || sending)
            .accessibilityIdentifier("send-follow-up")
            Text("The keyboard microphone is system dictation. It is not Voice Live.")
                .font(.caption2)
                .foregroundStyle(.tertiary)
        }
    }

    private func actions(_ agent: AgentInfo, _ machine: ServerInfo?) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            if agent.hasAgentUI {
                Button("Open Agent UI") { showUI = true }
                    .buttonStyle(.borderedProminent)
                    .disabled(machine?.isOnline != true)
            }
            if session.showTerminalControls || (!agent.hasAgentUI && !agent.canPrompt) {
                Button("Open terminal") { showTerminal = true }
                    .buttonStyle(.bordered)
                    .disabled(machine?.isOnline != true)
            }
            Button("Abort this turn") {
                session.presentConfirm(
                    ConfirmRequest(
                        action: .abort,
                        title: "Abort this turn",
                        objectName: agent.name,
                        objectId: agent.agentId,
                        machineHostname: machine?.hostname ?? machine?.displayName,
                        onConfirm: {
                            session.confirm = nil
                            Task { await session.abort(agentId: agent.agentId) }
                        },
                        onChange: { session.confirm = nil }
                    )
                )
            }
            .disabled(!agent.canAbort)
            if !agent.canAbort {
                Text("Abort is unavailable until this Agent advertises the abort capability.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Button("Stop process", role: .destructive) {
                session.presentConfirm(
                    ConfirmRequest(
                        action: .stop,
                        title: "Stop \(agent.name)",
                        objectName: agent.name,
                        objectId: agent.agentId,
                        machineHostname: machine?.hostname ?? machine?.displayName,
                        onConfirm: {
                            session.confirm = nil
                            Task { await session.stop(agentId: agent.agentId) }
                        },
                        onChange: { session.confirm = nil }
                    )
                )
            }
            Button("Delete Agent", role: .destructive) {
                session.presentConfirm(
                    ConfirmRequest(
                        action: .delete,
                        title: "Delete \(agent.name)",
                        objectName: agent.name,
                        objectId: agent.agentId,
                        machineHostname: machine?.hostname ?? machine?.displayName,
                        onConfirm: {
                            session.confirm = nil
                            Task { await session.delete(agentId: agent.agentId) }
                        },
                        onChange: { session.confirm = nil }
                    )
                )
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func sendFollowUp(_ agent: AgentInfo) {
        let text = draft
        let machine = session.snapshot?.machine(for: agent)
        if PromptPolicy.requiresConfirmation(text: text, agentStatus: agent.status) {
            session.presentConfirm(
                ConfirmRequest(
                    action: .prompt,
                    title: "Send follow-up",
                    objectName: agent.name,
                    objectId: agent.agentId,
                    machineHostname: machine?.hostname ?? machine?.displayName,
                    promptExcerpt: text,
                    onConfirm: {
                        session.confirm = nil
                        Task { await submit(agentId: agent.agentId, text: text) }
                    },
                    onChange: { session.confirm = nil }
                )
            )
        } else {
            Task { await submit(agentId: agent.agentId, text: text) }
        }
    }

    private func submit(agentId: String, text: String) async {
        sending = true
        defer { sending = false }
        do {
            try await session.sendPrompt(agentId: agentId, text: text, confirmed: true)
            draft = ""
            transcript = await session.loadTranscript(agentId: agentId)
        } catch {
            session.errorMessage = error.localizedDescription
        }
    }

    private func labeled(_ title: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Text(value).font(.body.monospaced())
                .textSelection(.enabled)
        }
    }
}

struct FlowBadges: View {
    var items: [(String, Bool)]

    var body: some View {
        FlexibleBadgeWrap(items: items)
    }
}

private struct FlexibleBadgeWrap: View {
    var items: [(String, Bool)]

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(Array(items.chunked(by: 3).enumerated()), id: \.offset) { _, row in
                HStack(spacing: 6) {
                    ForEach(row, id: \.0) { item in
                        Text(item.0)
                            .font(.caption2.weight(.medium))
                            .padding(.horizontal, 8)
                            .padding(.vertical, 4)
                            .background(item.1 ? TreerTheme.success.opacity(0.16) : Color.secondary.opacity(0.12), in: Capsule())
                            .foregroundStyle(item.1 ? TreerTheme.success : .secondary)
                    }
                }
            }
        }
    }
}

private extension Array {
    func chunked(by size: Int) -> [[Element]] {
        stride(from: 0, to: count, by: size).map { Array(self[$0 ..< Swift.min($0 + size, count)]) }
    }
}
