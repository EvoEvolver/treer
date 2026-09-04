import AVFoundation
import Foundation
import SwiftUI

enum AppPhase: Equatable {
    case proxySetup
    case login
    case register
    case resetPassword
    case workspaceSwitch
    case ready
}

@MainActor
final class AppSession: ObservableObject {
    @Published var phase: AppPhase = .proxySetup
    @Published var proxyURL: URL?
    @Published var user: UserProfile?
    @Published var authConfig = AuthConfig()
    @Published var organizations: [Organization] = []
    @Published var workspaces: [Workspace] = []
    @Published var selectedOrganization: Organization?
    @Published var selectedWorkspace: Workspace?
    @Published var snapshot: WorkspaceSnapshot?
    @Published var launchProfiles: [LaunchProfile] = []
    @Published var connection: ConnectionState = .idle
    @Published var stale = false
    @Published var errorMessage: String?
    @Published var busy = false
    @Published var confirm: ConfirmRequest?
    @Published var showVoicePreview = false
    @Published var showCreateAgent = false
    @Published var showAddMachine = false
    @Published var bootstrap: BootstrapInfo?
    @Published var createPrefillMachineId: String?
    @Published var openedAgentId: String?
    @Published var showSettings = false
    @Published var showWorkspaceSwitcher = false
    @Published var fixtureMode = false
    @Published var theme: AppTheme = .system
    @Published var showTerminalControls = false
    @Published var voiceAsr = VoiceAsrStatus()
    @Published var voiceCommand = VoiceCommandStatus()
    @Published var voiceReply: String?
    @Published var voiceBusy = false
    @Published var voiceLines: [VoiceLine] = []

    let settings: AppSettingsStore
    private var client: APIClient?
    private let events = EventSocket()
    private var snapshotRefresh: Task<Void, Never>?
    private let speech = AVSpeechSynthesizer()

    var isAuthenticated: Bool { user != nil && (fixtureMode || client?.token != nil) }
    var isOffline: Bool { connection == .offline || connection == .reconnecting }
    var apiClient: APIClient? { client }

    init(settings: AppSettingsStore = AppSettingsStore(), launchArguments: [String] = ProcessInfo.processInfo.arguments) {
        self.settings = settings
        fixtureMode = launchArguments.contains("-treer-fixture")
        if launchArguments.contains("-treer-reset") {
            settings.reset()
        }
        theme = settings.theme
        showTerminalControls = settings.showTerminalControls
        if fixtureMode {
            phase = .proxySetup
            return
        }
        if let stored = settings.proxyURLString, let url = URL(string: stored) {
            proxyURL = url
            let api = APIClient(proxyURL: url, token: try? settings.sessionToken())
            client = api
            if api.token != nil {
                phase = .login
                Task { await restoreSession() }
            } else {
                phase = .login
            }
        } else {
            phase = .proxySetup
        }
        events.onSnapshot = { [weak self] snapshot in
            Task { @MainActor in
                self?.applySnapshot(snapshot)
                self?.connection = .live
                self?.stale = false
            }
        }
        events.onState = { [weak self] state in
            Task { @MainActor in
                self?.connection = state
                if state == .offline || state == .reconnecting {
                    self?.stale = self?.snapshot != nil
                }
            }
        }
    }

    var proxyHostLabel: String {
        proxyURL?.absoluteString ?? settings.proxyURLString ?? "this Proxy"
    }

    func continueWithProxy(_ raw: String) async {
        errorMessage = nil
        if fixtureMode {
            proxyURL = URL(string: raw.hasPrefix("http") ? raw : "http://127.0.0.1:8080")
            settings.proxyURLString = proxyURL?.absoluteString
            phase = .login
            return
        }
        do {
            let url = try APIClient.normalizeProxyURL(raw)
            let api = APIClient(proxyURL: url)
            try await api.probeProxy()
            proxyURL = url
            settings.proxyURLString = url.absoluteString
            client = api
            authConfig = (try? await api.authConfig()) ?? AuthConfig()
            phase = .login
        } catch {
            errorMessage = (error as? LocalizedError)?.errorDescription ?? "Unable to reach a Treer Proxy at that URL."
        }
    }

    func login(email: String, password: String) async {
        if fixtureMode {
            enterFixture(user: UserProfile(userId: "usr_fixture", email: email, preferredName: "Operator"))
            return
        }
        await run {
            guard let client else { throw APIError(status: -1, message: "Set a Proxy URL first.", code: nil) }
            let session = try await client.login(
                email: email,
                password: password,
                deviceId: settings.deviceId(),
                deviceName: APIClient.deviceName()
            )
            try await self.storeSession(session, on: client)
        }
    }

    func register(email: String, password: String, preferredName: String, invite: String) async {
        if fixtureMode {
            enterFixture(user: UserProfile(userId: "usr_fixture", email: email, preferredName: preferredName))
            return
        }
        await run {
            guard let client else { throw APIError(status: -1, message: "Set a Proxy URL first.", code: nil) }
            let session = try await client.register(
                email: email,
                password: password,
                preferredName: preferredName,
                invite: invite.isEmpty ? nil : invite,
                deviceId: settings.deviceId(),
                deviceName: APIClient.deviceName()
            )
            try await self.storeSession(session, on: client)
        }
    }

    func requestPasswordReset(email: String) async {
        await run {
            guard let client else { throw APIError(status: -1, message: "Set a Proxy URL first.", code: nil) }
            try await client.requestPasswordReset(email: email)
        }
    }

    func resetPassword(token: String, password: String) async {
        await run {
            guard let client else { throw APIError(status: -1, message: "Set a Proxy URL first.", code: nil) }
            try await client.resetPassword(token: token, password: password)
            phase = .login
        }
    }

    func select(organization: Organization) async {
        selectedOrganization = organization
        settings.lastOrganizationId = organization.organizationId
        if fixtureMode {
            workspaces = [FixtureData.workspace]
            return
        }
        await run {
            guard let client else { return }
            workspaces = try await client.workspaces(organizationId: organization.organizationId)
        }
    }

    func select(workspace: Workspace) async {
        selectedWorkspace = workspace
        settings.lastWorkspaceId = workspace.workspaceId
        showWorkspaceSwitcher = false
        phase = .ready
        await refreshSnapshot()
        connectEvents()
        await refreshLaunchProfiles()
        await refreshVoiceAsr()
    }

    func refreshVoiceAsr() async {
        if fixtureMode {
            voiceAsr = VoiceAsrStatus()
            voiceCommand = VoiceCommandStatus()
            return
        }
        guard let client, let workspace = selectedWorkspace else {
            voiceAsr = VoiceAsrStatus()
            voiceCommand = VoiceCommandStatus()
            return
        }
        voiceAsr = (try? await client.voiceAsrStatus(workspaceId: workspace.workspaceId)) ?? VoiceAsrStatus()
        voiceCommand = (try? await client.voiceCommandStatus(workspaceId: workspace.workspaceId)) ?? VoiceCommandStatus()
    }

    func submitVoiceUtterance(_ text: String) async {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        guard let client, let workspace = selectedWorkspace else { return }
        let history = voiceLines
        voiceLines.append(VoiceLine(role: "user", text: trimmed))
        voiceBusy = true
        defer { voiceBusy = false }
        do {
            let result = try await client.voiceCommand(
                workspaceId: workspace.workspaceId,
                text: trimmed,
                history: history
            )
            voiceReply = result.reply
            voiceLines.append(VoiceLine(role: "assistant", text: result.reply))
            speak(result.reply)
        } catch {
            voiceReply = error.localizedDescription
            voiceLines.append(VoiceLine(role: "assistant", text: error.localizedDescription))
            speak(error.localizedDescription)
        }
    }

    func stopVoiceSpeech() {
        if speech.isSpeaking {
            speech.stopSpeaking(at: .immediate)
        }
    }

    private func speak(_ text: String) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        stopVoiceSpeech()
        let chinese = trimmed.unicodeScalars.contains { $0.value >= 0x4E00 && $0.value <= 0x9FFF }
        let language = chinese ? "zh-CN" : "en-US"
        guard AVSpeechSynthesisVoice(language: language) != nil || !AVSpeechSynthesisVoice.speechVoices().isEmpty else {
            errorMessage = "系统没有可用的朗读声音。请在「设置 › 辅助功能 › 朗读内容」里下载语音。"
            return
        }
        let utterance = AVSpeechUtterance(string: trimmed)
        utterance.voice = AVSpeechSynthesisVoice(language: language)
        utterance.rate = AVSpeechUtteranceDefaultSpeechRate
        speech.speak(utterance)
    }

    func createWorkspace(name: String) async {
        await run {
            guard let client, let organization = selectedOrganization else { return }
            let created = try await client.createWorkspace(organizationId: organization.organizationId, name: name)
            workspaces.append(created)
            await select(workspace: created)
        }
    }

    func refreshSnapshot() async {
        if fixtureMode {
            snapshot = FixtureData.snapshot
            connection = .live
            return
        }
        guard let client, let workspace = selectedWorkspace else { return }
        do {
            snapshot = try await client.snapshot(workspaceId: workspace.workspaceId)
            stale = false
        } catch let error as APIError where error.isUnauthorized {
            expireSession()
        } catch {
            stale = snapshot != nil
            errorMessage = (error as? LocalizedError)?.errorDescription
        }
    }

    func refreshLaunchProfiles() async {
        if fixtureMode {
            launchProfiles = [FixtureData.profile]
            return
        }
        guard let client, let workspace = selectedWorkspace else { return }
        launchProfiles = (try? await client.launchProfiles(workspaceId: workspace.workspaceId)) ?? []
    }

    func loadBootstrap() async {
        if fixtureMode {
            bootstrap = .fixture
            return
        }
        guard let client, let workspace = selectedWorkspace else { return }
        do {
            bootstrap = try await client.bootstrap(workspaceId: workspace.workspaceId)
        } catch let error as APIError where error.isUnauthorized {
            expireSession()
        } catch {
            errorMessage = (error as? LocalizedError)?.errorDescription ?? "Unable to load enrollment commands."
        }
    }

    func openAddMachine() {
        showCreateAgent = false
        showAddMachine = true
        Task { await loadBootstrap() }
    }

    func loadTranscript(agentId: String) async -> String? {
        if fixtureMode {
            return "Waiting for follow-up on the latest diff."
        }
        guard let client, let workspace = selectedWorkspace else { return nil }
        do {
            let response = try await client.transcript(workspaceId: workspace.workspaceId, agentId: agentId)
            return response.latestTurnText
        } catch {
            return nil
        }
    }

    func presentConfirm(_ request: ConfirmRequest) {
        confirm = request
    }

    func createAgent(machineId: String, source: CreateSource, name: String, firstPrompt: String?) async {
        await run {
            let agent: AgentInfo
            if fixtureMode {
                let kind: String
                switch source {
                case .terminal:
                    kind = "command"
                case let .kind(value):
                    kind = AgentCatalog.wireKind(value)
                case let .profile(profile):
                    kind = profile.inferredKind ?? profile.command
                }
                agent = AgentInfo(
                    agentId: "ag_\(UUID().uuidString.prefix(12))",
                    workspaceId: selectedWorkspace?.workspaceId,
                    serverId: machineId,
                    kind: kind,
                    name: name,
                    status: .starting,
                    interface: source.isAIS ? AgentInterface(capabilities: ["prompt.submit"], uiPath: "/ui") : nil
                )
            } else {
                guard let client, let workspace = selectedWorkspace else { return }
                switch source {
                case .terminal:
                    agent = try await client.createAgent(
                        workspaceId: workspace.workspaceId,
                        serverId: machineId,
                        kind: AgentCatalog.wireKind("terminal"),
                        name: name
                    )
                case let .kind(kind):
                    agent = try await client.createAgent(
                        workspaceId: workspace.workspaceId,
                        serverId: machineId,
                        kind: AgentCatalog.wireKind(kind),
                        name: name
                    )
                case let .profile(profile):
                    agent = try await client.launchProfile(
                        workspaceId: workspace.workspaceId,
                        profileId: profile.profileId,
                        serverId: machineId,
                        agentName: name
                    )
                }
            }
            showCreateAgent = false
            showAddMachine = false
            await refreshSnapshot()
            if let prompt = firstPrompt?.trimmingCharacters(in: .whitespacesAndNewlines), !prompt.isEmpty {
                try await sendPrompt(agentId: agent.agentId, text: prompt, confirmed: true)
            }
            openedAgentId = agent.agentId
        }
    }

    func sendPrompt(agentId: String, text: String, confirmed: Bool) async throws {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        if fixtureMode { return }
        guard let client, let workspace = selectedWorkspace else { return }
        if !confirmed, let agent = snapshot?.agents.first(where: { $0.agentId == agentId }),
           PromptPolicy.requiresConfirmation(text: trimmed, agentStatus: agent.status)
        {
            throw APIError(status: 0, message: "confirmation_required", code: "confirmation_required")
        }
        try await client.prompt(workspaceId: workspace.workspaceId, agentId: agentId, text: trimmed)
        await refreshSnapshot()
    }

    func abort(agentId: String) async {
        await run {
            guard let client, let workspace = selectedWorkspace else { return }
            try await client.abort(workspaceId: workspace.workspaceId, agentId: agentId)
            await refreshSnapshot()
        }
    }

    func stop(agentId: String) async {
        await run {
            guard let client, let workspace = selectedWorkspace else { return }
            try await client.stop(workspaceId: workspace.workspaceId, agentId: agentId)
            await refreshSnapshot()
        }
    }

    func delete(agentId: String) async {
        await run {
            guard let client, let workspace = selectedWorkspace else { return }
            try await client.deleteAgent(workspaceId: workspace.workspaceId, agentId: agentId)
            await refreshSnapshot()
        }
    }

    func updateProfile(email: String, preferredName: String) async {
        await run {
            guard let client else { return }
            user = try await client.updateProfile(email: email, preferredName: preferredName)
        }
    }

    func requestLogout() {
        presentConfirm(
            ConfirmRequest(
                action: .logout,
                title: "Sign out",
                objectName: proxyHostLabel,
                onConfirm: { [weak self] in
                    Task { await self?.logout() }
                }
            )
        )
    }

    func requestSwitchProxy(to raw: String) {
        presentConfirm(
            ConfirmRequest(
                action: .switchProxy,
                title: "Switch Proxy",
                objectName: proxyHostLabel,
                onConfirm: { [weak self] in
                    Task { await self?.switchProxy(raw) }
                }
            )
        )
    }

    func logout() async {
        confirm = nil
        if let client, !fixtureMode {
            try? await client.logout()
        }
        events.disconnect()
        try? settings.clearSessionToken()
        user = nil
        snapshot = nil
        selectedWorkspace = nil
        phase = proxyURL == nil ? .proxySetup : .login
        connection = .idle
    }

    func switchProxy(_ raw: String) async {
        confirm = nil
        await logout()
        settings.proxyURLString = nil
        proxyURL = nil
        client = nil
        phase = .proxySetup
        await continueWithProxy(raw)
    }

    func updateTheme(_ theme: AppTheme) {
        self.theme = theme
        settings.theme = theme
    }

    func updateShowTerminal(_ value: Bool) {
        showTerminalControls = value
        settings.showTerminalControls = value
    }

    func clientForTunnel() -> APIClient? {
        client
    }

    func expireSession() {
        events.disconnect()
        try? settings.clearSessionToken()
        user = nil
        phase = .login
        errorMessage = "Sign in again to continue."
    }

    private func restoreSession() async {
        guard let client else { return }
        do {
            user = try await client.me()
            try await loadGate()
        } catch {
            expireSession()
        }
    }

    private func storeSession(_ session: SessionResponse, on client: APIClient) async throws {
        guard let token = session.token, !token.isEmpty else {
            throw APIError(status: 500, message: "This Proxy did not return a native session token.", code: "missing_token")
        }
        client.token = token
        try settings.saveSessionToken(token)
        self.client = client
        user = session.user
        try await loadGate()
    }

    private func loadGate() async throws {
        guard let client else { return }
        organizations = try await client.organizations()
        if let lastOrg = settings.lastOrganizationId,
           let org = organizations.first(where: { $0.organizationId == lastOrg })
        {
            selectedOrganization = org
            workspaces = try await client.workspaces(organizationId: org.organizationId)
            if let lastWorkspace = settings.lastWorkspaceId,
               let workspace = workspaces.first(where: { $0.workspaceId == lastWorkspace })
            {
                await select(workspace: workspace)
                return
            }
        }
        phase = .workspaceSwitch
    }

    private func connectEvents() {
        events.disconnect()
        if fixtureMode {
            connection = .live
            return
        }
        guard let client, let workspace = selectedWorkspace,
              let request = try? client.eventsWebSocketRequest(workspaceId: workspace.workspaceId)
        else { return }
        events.connect(request: request)
    }

    private func applySnapshot(_ snapshot: WorkspaceSnapshot) {
        self.snapshot = snapshot
        if selectedWorkspace?.workspaceId == snapshot.workspace.workspaceId {
            selectedWorkspace = snapshot.workspace
        }
    }

    private func enterFixture(user: UserProfile) {
        self.user = user
        organizations = [FixtureData.organization]
        selectedOrganization = FixtureData.organization
        workspaces = [FixtureData.workspace]
        phase = .workspaceSwitch
    }

    private func run(_ work: () async throws -> Void) async {
        busy = true
        errorMessage = nil
        defer { busy = false }
        do {
            try await work()
        } catch let error as APIError where error.isUnauthorized {
            expireSession()
        } catch {
            errorMessage = (error as? LocalizedError)?.errorDescription ?? String(describing: error)
        }
    }
}

enum CreateSource: Equatable {
    case terminal
    case kind(String)
    case profile(LaunchProfile)

    var kindLabel: String {
        switch self {
        case .terminal:
            return "Terminal"
        case let .kind(kind):
            return AgentCatalog.label(for: kind)
        case let .profile(profile):
            return profile.inferredKind ?? profile.name
        }
    }

    var isAIS: Bool {
        switch self {
        case .terminal, .kind:
            return false
        case let .profile(profile):
            return profile.looksAISCapable
        }
    }
}
