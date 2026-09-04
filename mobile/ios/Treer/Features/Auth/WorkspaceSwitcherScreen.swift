import SwiftUI

struct WorkspaceSwitcherScreen: View {
    @EnvironmentObject private var session: AppSession
    var asSheet = false
    @State private var newWorkspaceName = ""

    var body: some View {
        NavigationStack {
            List {
                Section("Organization") {
                    if session.organizations.isEmpty {
                        Text("No organizations yet. Join from an invite or create one on the desktop.")
                            .foregroundStyle(.secondary)
                    } else {
                        ForEach(session.organizations) { org in
                            Button {
                                Task { await session.select(organization: org) }
                            } label: {
                                HStack {
                                    VStack(alignment: .leading) {
                                        Text(org.name)
                                        if let role = org.role {
                                            Text(role).font(.caption).foregroundStyle(.secondary)
                                        }
                                    }
                                    Spacer()
                                    if org.id == session.selectedOrganization?.id {
                                        Image(systemName: "checkmark")
                                    }
                                }
                            }
                            .accessibilityIdentifier("organization-\(org.name)")
                        }
                    }
                }
                if session.selectedOrganization != nil {
                    Section("Workspace") {
                        if session.workspaces.isEmpty {
                            Text("No workspaces in this organization.")
                                .foregroundStyle(.secondary)
                        }
                        ForEach(session.workspaces) { workspace in
                            Button {
                                Task { await session.select(workspace: workspace) }
                            } label: {
                                HStack {
                                    Text(workspace.name)
                                    Spacer()
                                    if workspace.id == session.selectedWorkspace?.id {
                                        Image(systemName: "checkmark")
                                    }
                                }
                            }
                            .accessibilityIdentifier("workspace-\(workspace.name)")
                        }
                    }
                    Section("Create workspace") {
                        TextField("Name", text: $newWorkspaceName)
                        Button("Create") {
                            let name = newWorkspaceName.trimmingCharacters(in: .whitespacesAndNewlines)
                            guard !name.isEmpty else { return }
                            Task { await session.createWorkspace(name: name) }
                        }
                        .disabled(newWorkspaceName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || session.busy)
                    }
                }
                if session.stale {
                    Section {
                        Text("Showing cached names. You can look, but creating a workspace needs a live Proxy.")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .navigationTitle("Workspaces")
            .toolbar {
                if asSheet {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Close") { session.showWorkspaceSwitcher = false }
                    }
                }
            }
            .task {
                if session.organizations.isEmpty, !session.fixtureMode {
                    // Gate already loaded organizations after login.
                }
            }
        }
    }
}
