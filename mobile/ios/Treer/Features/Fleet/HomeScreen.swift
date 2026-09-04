import SwiftUI

struct HomeScreen: View {
    @EnvironmentObject private var session: AppSession

    var body: some View {
        NavigationStack {
            Group {
                if let snapshot = session.snapshot {
                    snapshotView(snapshot)
                } else {
                    ContentUnavailableView("Loading lab", systemImage: "dot.radiowaves.left.and.right", description: Text("Fetching the workspace snapshot."))
                }
            }
            .navigationTitle("")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { FleetToolbar(plus: .assign, showsAssign: true) }
            .refreshable { await session.refreshSnapshot() }
            .navigationDestination(item: Binding(
                get: { session.openedAgentId },
                set: { session.openedAgentId = $0 }
            )) { agentId in
                AgentDetailScreen(agentId: agentId)
            }
        }
        .accessibilityIdentifier("home-screen")
        .accessibilityElement(children: .contain)
    }

    @ViewBuilder
    private func snapshotView(_ snapshot: WorkspaceSnapshot) -> some View {
        let attention = Attention.homeAttention(in: snapshot)
        let working = Attention.working(in: snapshot)
        let idle = Attention.idle(in: snapshot)
        List {
            fleetStrip(snapshot)
            if snapshot.servers.isEmpty {
                Section {
                    Text("No machines yet. Open Machines and tap + to copy install and connect commands. Run them on a computer — this phone does not run a Host.")
                        .foregroundStyle(.secondary)
                }
            } else if snapshot.agents.isEmpty {
                Section {
                    Text("No Agents yet. Tap + to start one on an online machine.")
                        .foregroundStyle(.secondary)
                }
            } else {
                Section("Needs attention") {
                    if attention.items.isEmpty {
                        Text("Nothing needs you right now.")
                            .foregroundStyle(.secondary)
                    } else {
                        ForEach(attention.items) { agent in
                            NavigationLink {
                                AgentDetailScreen(agentId: agent.agentId)
                            } label: {
                                AgentRow(agent: agent, machine: snapshot.machine(for: agent))
                            }
                        }
                        if attention.overflow > 0 {
                            Text("+\(attention.overflow) in Inbox")
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                Section("Working now") {
                    if working.isEmpty {
                        Text("No Agents are working.")
                            .foregroundStyle(.secondary)
                    } else {
                        ForEach(working) { agent in
                            NavigationLink {
                                AgentDetailScreen(agentId: agent.agentId)
                            } label: {
                                AgentRow(agent: agent, machine: snapshot.machine(for: agent))
                            }
                        }
                    }
                }
                idleSections(snapshot, idle: idle)
            }
        }
        .listStyle(.insetGrouped)
    }

    @ViewBuilder
    private func idleSections(_ snapshot: WorkspaceSnapshot, idle: [AgentInfo]) -> some View {
        let grouped = Dictionary(grouping: idle, by: \.serverId)
        Section("Idle & ready") {
            if idle.isEmpty {
                Text("No idle Agents. Assign work to an online machine.")
                    .foregroundStyle(.secondary)
            } else {
                ForEach(grouped.keys.sorted(), id: \.self) { serverId in
                    let name = snapshot.servers.first(where: { $0.serverId == serverId })?.displayName ?? serverId
                    Text(name)
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.secondary)
                    ForEach(grouped[serverId] ?? []) { agent in
                        NavigationLink {
                            AgentDetailScreen(agentId: agent.agentId)
                        } label: {
                            AgentRow(agent: agent, machine: snapshot.machine(for: agent), warningUnknown: true)
                        }
                    }
                }
            }
        }
    }

    private func fleetStrip(_ snapshot: WorkspaceSnapshot) -> some View {
        let online = snapshot.servers.filter(\.isOnline).count
        let working = snapshot.agents.filter { $0.status == .working || $0.status == .starting }.count
        let blocked = snapshot.agents.filter { $0.status == .blocked }.count
        let idle = snapshot.agents.filter { $0.status == .idle || $0.status == .unknown }.count
        return Section("Fleet") {
            Text("Online machines \(online)/\(snapshot.servers.count) · Agents working \(working) · blocked \(blocked) · idle \(idle)")
                .font(.footnote)
        }
    }
}

struct AgentRow: View {
    var agent: AgentInfo
    var machine: ServerInfo?
    var warningUnknown = false

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(agent.name)
                    .font(.headline)
                Spacer()
                StatusDot(value: agent.displayStatus(machine: machine), warningUnknown: warningUnknown)
            }
            Text(Attention.summary(for: agent, machine: machine))
                .font(.subheadline)
                .foregroundStyle(.secondary)
            HStack {
                Text(agent.kind)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text(machine?.displayName ?? agent.serverId)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                Text(agent.updatedAt, style: .relative)
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(.vertical, 4)
    }
}
