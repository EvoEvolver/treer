import SwiftUI

struct ProxySetupScreen: View {
    @EnvironmentObject private var session: AppSession
    @State private var url = ""
    @State private var checking = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("http://192.168.1.10:8080", text: $url)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .keyboardType(.URL)
                        .accessibilityIdentifier("proxy-url-field")
                } header: {
                    Text("Proxy URL")
                } footer: {
                    Text("This is the control plane address, not a machine’s LAN IP. First-time setup points Treer at the Proxy you already deployed.")
                }
                if let error = session.errorMessage {
                    Section {
                        Text(error)
                            .foregroundStyle(TreerTheme.danger)
                    }
                }
                Section {
                    Button {
                        checking = true
                        Task {
                            await session.continueWithProxy(url)
                            checking = false
                        }
                    } label: {
                        HStack {
                            Text("Continue")
                            if checking || session.busy {
                                Spacer()
                                ProgressView()
                            }
                        }
                    }
                    .disabled(url.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || checking)
                    .accessibilityIdentifier("proxy-continue")
                }
            }
            .navigationTitle("Treer")
            .onAppear {
                if url.isEmpty {
                    url = session.settings.proxyURLString ?? ""
                }
            }
        }
    }
}
