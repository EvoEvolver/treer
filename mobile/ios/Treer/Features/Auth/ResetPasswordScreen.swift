import SwiftUI

struct ResetPasswordScreen: View {
    @EnvironmentObject private var session: AppSession
    @State private var email = ""
    @State private var token = ""
    @State private var password = ""
    @State private var requested = false

    var body: some View {
        NavigationStack {
            Form {
                Section("Request a reset") {
                    TextField("Email", text: $email)
                        .textInputAutocapitalization(.never)
                        .keyboardType(.emailAddress)
                        .autocorrectionDisabled()
                    Button("Send reset email") {
                        Task {
                            await session.requestPasswordReset(email: email)
                            requested = session.errorMessage == nil
                        }
                    }
                    .disabled(email.isEmpty || session.busy)
                }
                if requested {
                    Section {
                        Text("If you do not receive email, ask the person who deployed this Proxy to confirm sending is configured. Do not assume the message was delivered.")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                }
                Section("Have a reset token?") {
                    TextField("Token", text: $token)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    SecureField("New password", text: $password)
                    Button("Reset password") {
                        Task { await session.resetPassword(token: token, password: password) }
                    }
                    .disabled(token.isEmpty || password.isEmpty || session.busy)
                }
                if let error = session.errorMessage {
                    Section { Text(error).foregroundStyle(TreerTheme.danger) }
                }
                Section {
                    Button("Back to sign in") {
                        session.errorMessage = nil
                        session.phase = .login
                    }
                }
            }
            .navigationTitle("Reset password")
        }
    }
}
