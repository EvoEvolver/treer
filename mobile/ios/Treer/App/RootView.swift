import SwiftUI

struct RootView: View {
    @EnvironmentObject private var session: AppSession

    var body: some View {
        Group {
            switch session.phase {
            case .proxySetup:
                ProxySetupScreen()
            case .login:
                LoginScreen()
            case .register:
                RegisterScreen()
            case .resetPassword:
                ResetPasswordScreen()
            case .workspaceSwitch:
                WorkspaceSwitcherScreen()
            case .ready:
                MainTabView()
            }
        }
        .tint(TreerTheme.accent)
        .sheet(item: $session.confirm) { request in
            ConfirmCardView(request: request) {
                session.confirm = nil
            }
        }
        .sheet(isPresented: $session.showVoicePreview) {
            VoicePreviewSheet()
        }
        .sheet(isPresented: $session.showSettings) {
            SettingsScreen()
        }
        .sheet(isPresented: $session.showCreateAgent) {
            CreateAgentSheet(prefillMachineId: session.createPrefillMachineId)
        }
        .sheet(isPresented: $session.showAddMachine) {
            AddMachineSheet()
        }
        .sheet(isPresented: $session.showWorkspaceSwitcher) {
            WorkspaceSwitcherScreen(asSheet: true)
        }
        .alert(
            "Something went wrong",
            isPresented: Binding(
                get: { session.errorMessage != nil && session.phase == .ready },
                set: { if !$0 { session.errorMessage = nil } }
            )
        ) {
            Button("OK", role: .cancel) { session.errorMessage = nil }
        } message: {
            Text(session.errorMessage ?? "")
        }
    }
}
