import SwiftUI

struct RegisterScreen: View {
    @EnvironmentObject private var session: AppSession
    @State private var preferredName = ""
    @State private var email = ""
    @State private var password = ""
    @State private var invite = ""

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Preferred name", text: $preferredName)
                    TextField("Email", text: $email)
                        .textInputAutocapitalization(.never)
                        .keyboardType(.emailAddress)
                        .autocorrectionDisabled()
                    SecureField("Password", text: $password)
                    if session.authConfig.invitationRequired {
                        TextField("Invite code", text: $invite)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                    }
                } footer: {
                    if session.authConfig.invitationRequired && invite.isEmpty {
                        Text("This Proxy requires an invitation. Ask a desktop admin if you do not have a code.")
                    }
                }
                if let error = session.errorMessage {
                    Section { Text(error).foregroundStyle(TreerTheme.danger) }
                }
                Section {
                    Button("Create account") {
                        Task {
                            await session.register(
                                email: email,
                                password: password,
                                preferredName: preferredName,
                                invite: invite
                            )
                        }
                    }
                    .disabled(email.isEmpty || password.isEmpty || preferredName.isEmpty || session.busy)
                    Button("Already have an account") {
                        session.errorMessage = nil
                        session.phase = .login
                    }
                }
            }
            .navigationTitle("Register")
        }
    }
}
