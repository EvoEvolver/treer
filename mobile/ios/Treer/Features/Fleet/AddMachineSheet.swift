import SwiftUI
import UIKit

struct AddMachineSheet: View {
    @EnvironmentObject private var session: AppSession
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Text("This phone does not run a Host. Copy both commands and run them on the computer you want to enroll. Step 1 installs Treer. Step 2 is a 10-minute, single-use enrollment key.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
                Section("1. Install") {
                    Text(session.bootstrap?.installCommand ?? "Loading…")
                        .font(.caption.monospaced())
                        .textSelection(.enabled)
                    Button("Copy install command") {
                        UIPasteboard.general.string = session.bootstrap?.installCommand
                    }
                    .disabled(session.bootstrap?.installCommand.isEmpty != false)
                    .accessibilityIdentifier("copy-install-command")
                }
                Section("2. Connect this workspace") {
                    Text(session.bootstrap?.connectCommand ?? "Loading…")
                        .font(.caption.monospaced())
                        .textSelection(.enabled)
                    Button("Copy connect command") {
                        UIPasteboard.general.string = session.bootstrap?.connectCommand
                    }
                    .disabled(session.bootstrap?.connectCommand.isEmpty != false)
                    .accessibilityIdentifier("copy-connect-command")
                }
                Section {
                    Button("I've run the command — refresh") {
                        Task {
                            await session.loadBootstrap()
                            await session.refreshSnapshot()
                        }
                    }
                    .accessibilityIdentifier("refresh-machines")
                    if let online = session.snapshot?.servers.filter(\.isOnline), !online.isEmpty {
                        Text("Online: \(online.map(\.displayName).joined(separator: ", "))")
                        Button("Assign an Agent") {
                            session.createPrefillMachineId = online.first?.serverId
                            session.showAddMachine = false
                            session.showCreateAgent = true
                        }
                        .accessibilityIdentifier("assign-after-enroll")
                    } else if session.snapshot?.servers.isEmpty == false {
                        Text("A machine is enrolled but offline. Open it from Machines to copy recovery commands.")
                            .foregroundStyle(.secondary)
                    } else {
                        Text("The machine appears here as soon as the Host connects.")
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .navigationTitle("Add machine")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
            .task {
                await session.loadBootstrap()
                while !Task.isCancelled {
                    try? await Task.sleep(nanoseconds: 4_000_000_000)
                    await session.refreshSnapshot()
                }
            }
            .refreshable {
                await session.refreshSnapshot()
            }
        }
        .accessibilityIdentifier("add-machine-screen")
    }
}
