import SwiftUI

enum TreerTheme {
    static let accent = Color(red: 0.18, green: 0.29, blue: 0.43)
    static let success = Color(red: 0.09, green: 0.40, blue: 0.29)
    static let warning = Color(red: 0.57, green: 0.25, blue: 0.05)
    static let danger = Color(red: 0.75, green: 0.07, blue: 0.24)
    static let info = Color(red: 0.03, green: 0.35, blue: 0.52)

    static func statusColor(_ status: String) -> Color {
        switch status {
        case "idle":
            return success
        case "working", "starting":
            return info
        case "blocked", "unknown":
            return warning
        case "failed", "exited", "offline":
            return danger
        default:
            return .secondary
        }
    }
}

struct StatusDot: View {
    var value: String
    var warningUnknown = false

    var body: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(TreerTheme.statusColor(value))
                .frame(width: 7, height: 7)
            Text(value)
                .font(.caption.weight(.medium))
                .foregroundStyle(TreerTheme.statusColor(value))
            if warningUnknown && value == "unknown" {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.caption2)
                    .foregroundStyle(TreerTheme.warning)
            }
        }
        .textCase(.lowercase)
    }
}

struct VoiceButton: View {
    var action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: "mic.fill")
                .font(.title3.weight(.semibold))
                .foregroundStyle(.white)
                .frame(width: 56, height: 56)
                .background(TreerTheme.accent, in: Circle())
                .shadow(color: .black.opacity(0.18), radius: 8, y: 3)
        }
        .accessibilityLabel("Voice")
        .accessibilityIdentifier("voice-button")
    }
}

enum FleetPlusAction {
    case none
    case assign
    case addMachine
}

struct FleetToolbar: ToolbarContent {
    @EnvironmentObject private var session: AppSession
    var plus: FleetPlusAction = .none
    var showsAssign = false

    var body: some ToolbarContent {
        ToolbarItem(placement: .topBarLeading) {
            Button {
                session.showWorkspaceSwitcher = true
            } label: {
                VStack(alignment: .leading, spacing: 1) {
                    Text(title)
                        .font(.subheadline.weight(.semibold))
                        .lineLimit(1)
                    HStack(spacing: 4) {
                        Circle()
                            .fill(connectionColor)
                            .frame(width: 6, height: 6)
                        Text(session.connection.rawValue)
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                        if session.stale {
                            Text("stale")
                                .font(.caption2)
                                .foregroundStyle(TreerTheme.warning)
                        }
                    }
                }
            }
            .accessibilityIdentifier("workspace-switcher")
        }
        ToolbarItemGroup(placement: .topBarTrailing) {
            if plus == .addMachine {
                Button {
                    session.openAddMachine()
                } label: {
                    Image(systemName: "plus")
                }
                .disabled(session.isOffline)
                .accessibilityIdentifier("add-machine-button")
                .accessibilityLabel("Add machine")
            } else if plus == .assign || showsAssign {
                Button {
                    session.createPrefillMachineId = nil
                    session.showCreateAgent = true
                } label: {
                    Image(systemName: "plus")
                }
                .disabled(session.isOffline)
                .accessibilityIdentifier("assign-button")
                .accessibilityLabel("Assign")
            }
            Button {
                session.showSettings = true
            } label: {
                Image(systemName: "gearshape")
            }
            .accessibilityIdentifier("settings-button")
            .accessibilityLabel("Settings")
        }
    }

    private var title: String {
        let org = session.selectedOrganization?.name ?? "Organization"
        let workspace = session.selectedWorkspace?.name ?? "Workspace"
        return "\(org) / \(workspace)"
    }

    private var connectionColor: Color {
        switch session.connection {
        case .live:
            return TreerTheme.success
        case .reconnecting, .connecting:
            return TreerTheme.warning
        case .offline, .idle:
            return TreerTheme.danger
        }
    }
}

extension View {
    func treerScreen() -> some View {
        background(Color(.systemGroupedBackground).ignoresSafeArea())
    }
}
