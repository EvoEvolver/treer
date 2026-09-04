import SwiftUI

struct LoginScreen: View {
    @EnvironmentObject private var session: AppSession
    @State private var email = ""
    @State private var password = ""

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Text(session.proxyURL?.absoluteString ?? "")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                    Button("Change Proxy URL") {
                        session.phase = .proxySetup
                    }
                }
                Section("Account") {
                    TextField("Email", text: $email)
                        .textInputAutocapitalization(.never)
                        .keyboardType(.emailAddress)
                        .autocorrectionDisabled()
                        .accessibilityIdentifier("login-email")
                    SecureField("Password", text: $password)
                        .accessibilityIdentifier("login-password")
                }
                if session.authConfig.invitationRequired {
                    Section {
                        Text("This Proxy requires an invitation to register.")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                }
                if let error = session.errorMessage {
                    Section {
                        Text(error).foregroundStyle(TreerTheme.danger)
                    }
                }
                Section {
                    Button {
                        Task { await session.login(email: email, password: password) }
                    } label: {
                        HStack {
                            Text("Sign in")
                            if session.busy {
                                Spacer()
                                ProgressView()
                            }
                        }
                    }
                    .disabled(email.isEmpty || password.isEmpty || session.busy)
                    .accessibilityIdentifier("login-submit")
                    Button("Create account") {
                        session.errorMessage = nil
                        session.phase = .register
                    }
                    Button("Forgot password") {
                        session.errorMessage = nil
                        session.phase = .resetPassword
                    }
                }
            }
            .navigationTitle("Sign in")
        }
    }
}
