import SwiftUI
import UIKit

struct MachineDetailScreen: View {
    @EnvironmentObject private var session: AppSession
    var serverId: String

    var body: some View {
        let snapshot = session.snapshot
        let machine = snapshot?.machine(id: serverId)
        List {
            if let machine {
                Section("Status") {
                    StatusDot(value: machine.status.rawValue)
                    labeled("Hostname", machine.hostname)
                    labeled("Root", machine.root)
                    if let supervision = machine.supervision {
                        labeled("Supervision", supervision.displayMode)
                        if let reason = supervision.fallbackReason, !reason.isEmpty {
                            Text(reason).font(.footnote).foregroundStyle(.secondary)
                        }
                    }
                    if let controller = machine.controllerBuild {
                        labeled("Controller", controller.shortLabel)
                    }
                    if let host = machine.hostBuild {
                        labeled("Host", host.shortLabel)
                    }
                }
                if !machine.isOnline, let workspaceId = session.selectedWorkspace?.workspaceId {
                    Section("Recovery") {
                        Text("The protocol only reports online or offline. If this machine is asleep, stopped, or a fenced duplicate, copy a recovery command and run it there. restart-controller keeps Agents; start launches a stopped Host.")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                        recoveryRow("restart-controller", MachineRecovery.restartController(workspaceId: workspaceId))
                        recoveryRow("start", MachineRecovery.start(workspaceId: workspaceId))
                    }
                }
                Section("Agents") {
                    let agents = snapshot?.agents.filter { $0.serverId == serverId } ?? []
                    if agents.isEmpty {
                        Text("No Agents on this machine.")
                            .foregroundStyle(.secondary)
                    } else {
                        ForEach(agents) { agent in
                            NavigationLink {
                                AgentDetailScreen(agentId: agent.agentId)
                            } label: {
                                AgentRow(agent: agent, machine: machine, warningUnknown: true)
                            }
                        }
                    }
                }
                Section {
                    Button {
                        session.createPrefillMachineId = serverId
                        session.showCreateAgent = true
                    } label: {
                        Label("Assign on this machine", systemImage: "plus")
                    }
                    .disabled(machine.isOnline == false || session.isOffline)
                    .accessibilityIdentifier("assign-button")
                }
            } else {
                ContentUnavailableView("Machine unavailable", systemImage: "desktopcomputer")
            }
        }
        .navigationTitle(machine?.displayName ?? "Machine")
    }

    private func labeled(_ title: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Text(value).font(.body)
        }
    }

    private func recoveryRow(_ title: String, _ command: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(command)
                .font(.caption.monospaced())
                .textSelection(.enabled)
            Button("Copy \(title)") {
                UIPasteboard.general.string = command
            }
        }
    }
}
