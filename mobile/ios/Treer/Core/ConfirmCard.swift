import Foundation
import SwiftUI

enum ConfirmAction: String, CaseIterable, Equatable, Identifiable {
    case create
    case prompt
    case abort
    case stop
    case delete
    case launch
    case switchProxy = "switch_proxy"
    case logout

    var id: String { rawValue }

    var allowsChange: Bool {
        switch self {
        case .switchProxy, .logout:
            return false
        default:
            return true
        }
    }

    var isDestructive: Bool {
        switch self {
        case .stop, .delete, .switchProxy, .logout:
            return true
        default:
            return false
        }
    }

    func consequence(kindOrProfile: String? = nil, machine: String? = nil, name: String? = nil) -> String {
        switch self {
        case .abort:
            return "Cancel the current turn. The Agent process stays running."
        case .stop:
            return "Stop the process. You can Launch again. Transcript/PTY history follows Host retention."
        case .delete:
            return "Remove this Agent from the workspace. The process is stopped and the workspace entry is deleted."
        case .create, .launch:
            let kind = kindOrProfile ?? "agent"
            let host = machine ?? "the selected machine"
            let agentName = name ?? "this Agent"
            return "Start \(kind) on \(host) as \(agentName)."
        case .prompt:
            let agentName = name ?? "this Agent"
            let host = machine ?? "its machine"
            return "Send this follow-up to \(agentName) on \(host)."
        case .switchProxy:
            return "Leave this control plane and clear the Keychain session. You will sign in to the new Proxy URL. Agents on the old Proxy keep running."
        case .logout:
            return "Sign out this device. Other devices stay signed in. The Agent fleet is unchanged."
        }
    }
}

struct ConfirmRequest: Identifiable {
    var id = UUID()
    var action: ConfirmAction
    var title: String
    var objectName: String
    var objectId: String?
    var machineHostname: String?
    var promptExcerpt: String?
    var kindOrProfile: String?
    var onConfirm: () -> Void
    var onChange: (() -> Void)?

    var objectIdSuffix: String? {
        objectId.map(ObjectIDSuffix.suffix(for:))
    }

    var truncatedPrompt: String? {
        guard let promptExcerpt, !promptExcerpt.isEmpty else { return nil }
        if promptExcerpt.count <= 80 {
            return promptExcerpt
        }
        return String(promptExcerpt.prefix(80)) + "…"
    }

    var consequence: String {
        action.consequence(kindOrProfile: kindOrProfile, machine: machineHostname, name: objectName)
    }
}

struct ConfirmCardView: View {
    let request: ConfirmRequest
    var onCancel: () -> Void

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 16) {
                Text(request.title)
                    .font(.title2.weight(.semibold))
                detailRow("Name", request.objectName)
                if let suffix = request.objectIdSuffix {
                    detailRow("ID", "…" + suffix)
                }
                if let machine = request.machineHostname, !machine.isEmpty {
                    detailRow("Machine", machine)
                }
                if let excerpt = request.truncatedPrompt {
                    VStack(alignment: .leading, spacing: 6) {
                        Text("Prompt")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Text(excerpt)
                            .font(.callout)
                    }
                }
                Text(request.consequence)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .padding(.top, 4)
                Spacer()
                HStack {
                    Button("Cancel", action: onCancel)
                    Spacer()
                    if request.action.allowsChange, let onChange = request.onChange {
                        Button("Change", action: onChange)
                    }
                    Button("Confirm") {
                        request.onConfirm()
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(request.action.isDestructive ? .red : .accentColor)
                    .accessibilityIdentifier("confirm-action")
                }
            }
            .padding(20)
            .navigationTitle("Confirm")
            .navigationBarTitleDisplayMode(.inline)
        }
        .presentationDetents([.medium, .large])
    }

    private func detailRow(_ label: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label)
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(value)
                .font(.body.weight(.medium))
        }
    }
}
