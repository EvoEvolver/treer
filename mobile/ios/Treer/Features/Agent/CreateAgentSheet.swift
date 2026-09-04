import SwiftUI

struct CreateAgentSheet: View {
    @EnvironmentObject private var session: AppSession
    @Environment(\.dismiss) private var dismiss
    var prefillMachineId: String?
    @State private var machineId: String = ""
    @State private var source: CreateSource = .terminal
    @State private var name = ""
    @State private var firstPrompt = ""
    @State private var customizedName = false

    var body: some View {
        NavigationStack {
            Form {
                Section("Machine") {
                    let machines = session.snapshot?.servers.filter(\.isOnline) ?? []
                    if machines.isEmpty {
                        Text(
                            (session.snapshot?.servers.isEmpty ?? true)
                                ? "No machines enrolled yet. Add one from this phone, then Assign can start an Agent."
                                : "No online machines. Recover a Host from Machines, or add another."
                        )
                        .foregroundStyle(.secondary)
                        Button("Add a machine") {
                            session.openAddMachine()
                            dismiss()
                        }
                        .accessibilityIdentifier("add-machine-cta")
                    } else {
                        Picker("Machine", selection: $machineId) {
                            ForEach(machines) { machine in
                                Text(machine.displayName).tag(machine.serverId)
                            }
                        }
                    }
                }
                Section("What to run") {
                    Picker("Launch", selection: Binding(
                        get: { sourceTag },
                        set: { selectSource($0) }
                    )) {
                        ForEach(sortedSources, id: \.tag) { item in
                            Text(item.title).tag(item.tag)
                        }
                    }
                    .accessibilityIdentifier("create-source-picker")
                    sourceDetail
                }
                Section("Name") {
                    TextField("Name", text: $name)
                        .onChange(of: name) { _, _ in customizedName = true }
                }
                Section("First prompt (optional)") {
                    TextField("Ask the Agent to start with…", text: $firstPrompt, axis: .vertical)
                        .lineLimit(3 ... 6)
                }
            }
            .navigationTitle("Assign")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Create") { confirmCreate() }
                        .disabled(!canCreate || session.busy)
                        .accessibilityIdentifier("create-submit")
                }
            }
            .onAppear {
                let machines = session.snapshot?.servers.filter(\.isOnline) ?? []
                if let prefill = prefillMachineId, machines.contains(where: { $0.serverId == prefill }) {
                    machineId = prefill
                } else if machineId.isEmpty {
                    machineId = machines.first?.serverId ?? ""
                }
                if !customizedName {
                    applyPreferredSource()
                }
            }
            .onChange(of: machineId) { _, _ in
                if !customizedName {
                    applyPreferredSource()
                }
            }
        }
        .accessibilityIdentifier("create-agent-screen")
    }

    private var selectedMachine: ServerInfo? {
        session.snapshot?.machine(id: machineId)
    }

    private var sortedSources: [(tag: String, title: String, source: CreateSource)] {
        let profiles = session.launchProfiles.sorted { lhs, rhs in
            if lhs.looksAISCapable != rhs.looksAISCapable {
                return lhs.looksAISCapable && !rhs.looksAISCapable
            }
            return lhs.name.localizedCaseInsensitiveCompare(rhs.name) == .orderedAscending
        }
        var items: [(tag: String, title: String, source: CreateSource)] = profiles.map { profile in
            let installed = profile.inferredKind.flatMap { AgentCatalog.isInstalled(kind: $0, on: selectedMachine) }
            let suffix = installed == false ? " (install on machine)" : (profile.looksAISCapable ? " · AIS" : "")
            return (profile.profileId, profile.name + suffix, .profile(profile))
        }
        for entry in AgentCatalog.entries {
            let installed = AgentCatalog.isInstalled(kind: entry.kind, on: selectedMachine)
            let suffix = installed == false ? " (install on machine)" : " · TUI"
            items.append((entry.kind, entry.label + suffix, .kind(entry.kind)))
        }
        items.append(("terminal", "Terminal", .terminal))
        return items
    }

    private var sourceTag: String {
        switch source {
        case .terminal:
            return "terminal"
        case let .kind(kind):
            return kind
        case let .profile(profile):
            return profile.profileId
        }
    }

    private func selectSource(_ tag: String) {
        if tag == "terminal" {
            source = .terminal
            if !customizedName {
                name = AgentNaming.defaultName(kind: "terminal")
            }
            return
        }
        if let profile = session.launchProfiles.first(where: { $0.profileId == tag }) {
            source = .profile(profile)
            if !customizedName {
                name = AgentNaming.defaultProfileName(profile.name)
            }
            return
        }
        if AgentCatalog.kinds.contains(tag) {
            source = .kind(tag)
            if !customizedName {
                name = AgentNaming.defaultName(kind: tag)
            }
        }
    }

    private func applyPreferredSource() {
        let preferred = AgentCatalog.preferredKind(on: selectedMachine)
        selectSource(preferred)
        if name.isEmpty {
            name = AgentNaming.defaultName(kind: preferred)
        }
    }

    @ViewBuilder
    private var sourceDetail: some View {
        switch source {
        case .terminal:
            Text("Starts a built-in terminal session. Open the emergency TUI after create if you need a PTY.")
                .font(.footnote)
                .foregroundStyle(.secondary)
        case let .kind(kind):
            VStack(alignment: .leading, spacing: 6) {
                Text("Launches the \(AgentCatalog.label(for: kind)) CLI on the selected machine.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                if AgentCatalog.isInstalled(kind: kind, on: selectedMachine) == false {
                    Text("This CLI is not reported on the machine. Install it from the desktop; the phone does not run install scripts.")
                        .font(.footnote)
                        .foregroundStyle(TreerTheme.warning)
                }
            }
        case let .profile(profile):
            VStack(alignment: .leading, spacing: 6) {
                Text("\(profile.command) \(profile.args.joined(separator: " "))")
                    .font(.caption.monospaced())
                Text(profile.cwd.isEmpty ? "." : profile.cwd)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                if let kind = profile.inferredKind, AgentCatalog.isInstalled(kind: kind, on: selectedMachine) == false {
                    Text("This CLI is not reported on the machine. Install it from the desktop; the phone does not run install scripts.")
                        .font(.footnote)
                        .foregroundStyle(TreerTheme.warning)
                }
            }
        }
    }

    private var missingCLI: Bool {
        switch source {
        case let .kind(kind):
            return AgentCatalog.isInstalled(kind: kind, on: selectedMachine) == false
        case let .profile(profile):
            if let kind = profile.inferredKind {
                return AgentCatalog.isInstalled(kind: kind, on: selectedMachine) == false
            }
            return false
        case .terminal:
            return false
        }
    }

    private var canCreate: Bool {
        !machineId.isEmpty && !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !missingCLI
    }

    private func confirmCreate() {
        let machine = selectedMachine
        session.presentConfirm(
            ConfirmRequest(
                action: source.isAIS ? .launch : .create,
                title: source.isAIS ? "Start \(source.kindLabel)" : "Start \(source.kindLabel)",
                objectName: name,
                objectId: {
                    if case let .profile(profile) = source { return profile.profileId }
                    return nil
                }(),
                machineHostname: machine?.hostname ?? machine?.displayName,
                promptExcerpt: firstPrompt,
                kindOrProfile: source.kindLabel,
                onConfirm: {
                    session.confirm = nil
                    Task {
                        await session.createAgent(
                            machineId: machineId,
                            source: source,
                            name: name,
                            firstPrompt: firstPrompt
                        )
                        dismiss()
                    }
                },
                onChange: {
                    session.confirm = nil
                }
            )
        )
    }
}
