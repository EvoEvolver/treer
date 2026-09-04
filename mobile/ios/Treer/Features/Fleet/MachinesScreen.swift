import SwiftUI
import UIKit

struct MachinesScreen: View {
    @EnvironmentObject private var session: AppSession

    var body: some View {
        NavigationStack {
            List {
                if let snapshot = session.snapshot {
                    if snapshot.servers.isEmpty {
                        ContentUnavailableView(
                            "No machines",
                            systemImage: "desktopcomputer.trianglebadge.exclamationmark",
                            description: Text("Tap + to copy install and connect commands for a computer.")
                        )
                    } else {
                        ForEach(snapshot.servers) { machine in
                            NavigationLink {
                                MachineDetailScreen(serverId: machine.serverId)
                            } label: {
                                MachineRow(machine: machine, snapshot: snapshot)
                            }
                            .contextMenu {
                                Button("Copy server id") {
                                    UIPasteboard.general.string = machine.serverId
                                }
                                if let workspaceId = session.selectedWorkspace?.workspaceId {
                                    Button("Copy restart-controller") {
                                        UIPasteboard.general.string = MachineRecovery.restartController(workspaceId: workspaceId)
                                    }
                                }
                            }
                        }
                    }
                } else {
                    ProgressView()
                }
            }
            .navigationTitle("")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { FleetToolbar(plus: .addMachine) }
            .refreshable { await session.refreshSnapshot() }
        }
    }
}

struct MachineRow: View {
    var machine: ServerInfo
    var snapshot: WorkspaceSnapshot

    var body: some View {
        let agents = snapshot.agents.filter { $0.serverId == machine.serverId }
        let working = agents.filter { $0.status == .working || $0.status == .starting }.count
        let blocked = agents.filter { $0.status == .blocked }.count
        let idle = agents.filter { $0.status == .idle || $0.status == .unknown }.count
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(machine.displayName)
                    .font(.headline)
                Spacer()
                StatusDot(value: machine.status.rawValue)
            }
            Text(machine.hostname)
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
            Text("Agents working \(working) · blocked \(blocked) · idle \(idle)")
                .font(.caption)
                .foregroundStyle(.secondary)
            if let kinds = machine.availableAgents, !kinds.isEmpty {
                Text(kinds.joined(separator: " · "))
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(.vertical, 4)
    }
}
