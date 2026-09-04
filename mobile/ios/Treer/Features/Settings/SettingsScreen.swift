import SwiftUI
import UIKit

struct SettingsScreen: View {
    @EnvironmentObject private var session: AppSession
    @Environment(\.dismiss) private var dismiss
    @State private var proxyDraft = ""
    @State private var preferredName = ""
    @State private var email = ""

    var body: some View {
        NavigationStack {
            Form {
                Section("Proxy") {
                    TextField("Proxy URL", text: $proxyDraft)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .keyboardType(.URL)
                    Text(session.connection.rawValue)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                    Button("Switch Proxy") {
                        session.requestSwitchProxy(to: proxyDraft)
                    }
                    .disabled(proxyDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || session.isOffline)
                }
                Section("Account") {
                    TextField("Preferred name", text: $preferredName)
                    TextField("Email", text: $email)
                        .textInputAutocapitalization(.never)
                        .keyboardType(.emailAddress)
                    Button("Save") {
                        Task { await session.updateProfile(email: email, preferredName: preferredName) }
                    }
                    .disabled(session.isOffline)
                    Button("Sign out", role: .destructive) {
                        session.requestLogout()
                    }
                }
                Section("Appearance") {
                    Picker("Theme", selection: Binding(
                        get: { session.theme },
                        set: { session.updateTheme($0) }
                    )) {
                        ForEach(AppTheme.allCases) { theme in
                            Text(theme.title).tag(theme)
                        }
                    }
                }
                Section("Notifications") {
                    Text("Notifications unavailable until this Proxy has APNs/FCM configured.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                    Toggle("Blocked / failed alerts", isOn: .constant(false))
                        .disabled(true)
                }
                Section("Voice") {
                    if session.voiceAsr.enabled {
                        Text(session.voiceCommand.enabled
                             ? "Hold-to-talk ASR is on (\(session.voiceAsr.provider ?? "qwen")). Spoken text is sent to Treer command on this Proxy."
                             : "Hold-to-talk ASR is on (\(session.voiceAsr.provider ?? "qwen")). Command LLM is off until TREER_VOICE_LLM_API_KEY is set on the Proxy.")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    } else {
                        Text("Voice Live unavailable until this Proxy has a voice provider.")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                    Text("The Voice button on Home, Machines, and Inbox opens Preview. System dictation is only the composer keyboard.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
                Section("Advanced") {
                    Toggle("Show terminal controls", isOn: Binding(
                        get: { session.showTerminalControls },
                        set: { session.updateShowTerminal($0) }
                    ))
                    if let user = session.user {
                        labeled("User ID", user.userId)
                    }
                    if let workspace = session.selectedWorkspace {
                        labeled("Workspace ID", workspace.workspaceId)
                    }
                    Button("Copy diagnostics") {
                        UIPasteboard.general.string = diagnostics
                    }
                }
                Section("Usage & billing") {
                    Text("Usage and billing are not available yet. This control plane does not meter seats or invoices.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Settings")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
            .onAppear {
                proxyDraft = session.proxyURL?.absoluteString ?? session.settings.proxyURLString ?? ""
                preferredName = session.user?.preferredName ?? ""
                email = session.user?.email ?? ""
            }
        }
    }

    private var diagnostics: String {
        [
            "user_id=\(session.user?.userId ?? "")",
            "workspace_id=\(session.selectedWorkspace?.workspaceId ?? "")",
            "connection=\(session.connection.rawValue)",
            "revision=\(session.snapshot.map { String($0.revision) } ?? "")"
        ].joined(separator: "\n")
    }

    private func labeled(_ title: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Text(value).font(.footnote.monospaced()).textSelection(.enabled)
        }
    }
}
