import Foundation

struct UserProfile: Codable, Equatable, Identifiable {
    var userId: String
    var email: String
    var preferredName: String

    var id: String { userId }

    enum CodingKeys: String, CodingKey {
        case userId = "user_id"
        case email
        case preferredName = "preferred_name"
        case token
    }

    init(userId: String, email: String, preferredName: String) {
        self.userId = userId
        self.email = email
        self.preferredName = preferredName
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        userId = try container.decode(String.self, forKey: .userId)
        email = try container.decode(String.self, forKey: .email)
        preferredName = try container.decodeIfPresent(String.self, forKey: .preferredName) ?? ""
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(userId, forKey: .userId)
        try container.encode(email, forKey: .email)
        try container.encode(preferredName, forKey: .preferredName)
    }
}

struct SessionResponse: Decodable {
    var user: UserProfile
    var token: String?

    enum CodingKeys: String, CodingKey {
        case userId = "user_id"
        case email
        case preferredName = "preferred_name"
        case token
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        user = UserProfile(
            userId: try container.decode(String.self, forKey: .userId),
            email: try container.decode(String.self, forKey: .email),
            preferredName: try container.decodeIfPresent(String.self, forKey: .preferredName) ?? ""
        )
        token = try container.decodeIfPresent(String.self, forKey: .token)
    }
}

struct AuthConfig: Codable, Equatable {
    var github: Bool
    var google: Bool
    var invitationRequired: Bool

    enum CodingKeys: String, CodingKey {
        case github
        case google
        case invitationRequired = "invitation_required"
    }

    init(github: Bool = false, google: Bool = false, invitationRequired: Bool = false) {
        self.github = github
        self.google = google
        self.invitationRequired = invitationRequired
    }
}

struct VoiceAsrStatus: Decodable, Equatable {
    var enabled: Bool
    var provider: String?
    var sampleRate: Int
    var encoding: String

    enum CodingKeys: String, CodingKey {
        case enabled
        case provider
        case sampleRate = "sample_rate"
        case encoding
    }

    init(enabled: Bool = false, provider: String? = nil, sampleRate: Int = 16000, encoding: String = "pcm16") {
        self.enabled = enabled
        self.provider = provider
        self.sampleRate = sampleRate
        self.encoding = encoding
    }
}

struct VoiceCommandStatus: Decodable, Equatable {
    var enabled: Bool
    var wireApi: String?
    var model: String?

    enum CodingKeys: String, CodingKey {
        case enabled
        case wireApi = "wire_api"
        case model
    }

    init(enabled: Bool = false, wireApi: String? = nil, model: String? = nil) {
        self.enabled = enabled
        self.wireApi = wireApi
        self.model = model
    }
}

struct VoiceCommandReply: Decodable {
    var reply: String
    var utterance: String
}

struct VoiceLine: Identifiable, Equatable {
    let id: UUID
    var role: String
    var text: String

    init(role: String, text: String) {
        self.id = UUID()
        self.role = role
        self.text = text
    }
}

struct HealthStatus: Decodable {
    var status: String?
    var service: String?
}

struct Organization: Codable, Equatable, Identifiable {
    var organizationId: String
    var name: String
    var role: String?

    var id: String { organizationId }

    enum CodingKeys: String, CodingKey {
        case organizationId = "organization_id"
        case name
        case role
    }
}

struct Workspace: Codable, Equatable, Identifiable {
    var workspaceId: String
    var name: String
    var createdAt: Date?

    var id: String { workspaceId }

    enum CodingKeys: String, CodingKey {
        case workspaceId = "workspace_id"
        case name
        case createdAt = "created_at"
    }
}

enum ServerStatus: String, Codable, Equatable {
    case online
    case offline
}

enum AgentStatus: String, Codable, Equatable {
    case starting
    case working
    case idle
    case blocked
    case exited
    case failed
    case unknown

    var isTerminal: Bool {
        self == .exited || self == .failed
    }

    var isNonTerminal: Bool {
        switch self {
        case .starting, .working, .idle, .blocked, .unknown:
            return true
        case .exited, .failed:
            return false
        }
    }
}

struct BuildInfo: Codable, Equatable {
    var version: String
    var gitCommit: String

    enum CodingKeys: String, CodingKey {
        case version
        case gitCommit = "git_commit"
    }

    var shortLabel: String {
        let commit = gitCommit == "unknown" ? gitCommit : String(gitCommit.prefix(8))
        return "\(version)@\(commit)"
    }
}

struct MachineSupervision: Codable, Equatable {
    var mode: String
    var fallbackReason: String?

    enum CodingKeys: String, CodingKey {
        case mode
        case fallbackReason = "fallback_reason"
    }

    var displayMode: String {
        switch mode {
        case "systemd_user":
            return "systemd user"
        case "launchd":
            return "launchd"
        default:
            return mode
        }
    }
}

struct ServerInfo: Decodable, Equatable, Identifiable {
    var serverId: String
    var workspaceId: String?
    var name: String
    var hostname: String
    var root: String
    var controllerBuild: BuildInfo?
    var hostBuild: BuildInfo?
    var supervision: MachineSupervision?
    var labels: [String: String]
    var availableAgents: [String]?
    var status: ServerStatus
    var connectedAt: Date?
    var lastSeenAt: Date?

    var id: String { serverId }

    enum CodingKeys: String, CodingKey {
        case serverId = "server_id"
        case workspaceId = "workspace_id"
        case name
        case hostname
        case root
        case controllerBuild = "controller_build"
        case hostBuild = "host_build"
        case supervision
        case labels
        case availableAgents = "available_agents"
        case status
        case connectedAt = "connected_at"
        case lastSeenAt = "last_seen_at"
    }

    init(
        serverId: String,
        workspaceId: String? = nil,
        name: String = "",
        hostname: String = "",
        root: String = "",
        controllerBuild: BuildInfo? = nil,
        hostBuild: BuildInfo? = nil,
        supervision: MachineSupervision? = nil,
        labels: [String: String] = [:],
        availableAgents: [String]? = nil,
        status: ServerStatus,
        connectedAt: Date? = nil,
        lastSeenAt: Date? = nil
    ) {
        self.serverId = serverId
        self.workspaceId = workspaceId
        self.name = name
        self.hostname = hostname
        self.root = root
        self.controllerBuild = controllerBuild
        self.hostBuild = hostBuild
        self.supervision = supervision
        self.labels = labels
        self.availableAgents = availableAgents
        self.status = status
        self.connectedAt = connectedAt
        self.lastSeenAt = lastSeenAt
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        serverId = try container.decode(String.self, forKey: .serverId)
        workspaceId = try container.decodeIfPresent(String.self, forKey: .workspaceId)
        name = try container.decodeIfPresent(String.self, forKey: .name) ?? ""
        hostname = try container.decodeIfPresent(String.self, forKey: .hostname) ?? ""
        root = try container.decodeIfPresent(String.self, forKey: .root) ?? ""
        controllerBuild = try container.decodeIfPresent(BuildInfo.self, forKey: .controllerBuild)
        hostBuild = try container.decodeIfPresent(BuildInfo.self, forKey: .hostBuild)
        supervision = try container.decodeIfPresent(MachineSupervision.self, forKey: .supervision)
        labels = try container.decodeIfPresent([String: String].self, forKey: .labels) ?? [:]
        availableAgents = try container.decodeIfPresent([String].self, forKey: .availableAgents)
        status = try container.decodeIfPresent(ServerStatus.self, forKey: .status) ?? .offline
        connectedAt = try container.decodeIfPresent(Date.self, forKey: .connectedAt)
        lastSeenAt = try container.decodeIfPresent(Date.self, forKey: .lastSeenAt)
    }

    var displayName: String {
        if !name.isEmpty { return name }
        if !hostname.isEmpty { return hostname }
        return serverId
    }

    var isOnline: Bool { status == .online }
}

struct AgentInterface: Decodable, Equatable {
    var protocolName: String
    var instanceId: String
    var port: UInt16
    var capabilities: [String]
    var uiPath: String?
    var registeredAt: Date?

    enum CodingKeys: String, CodingKey {
        case protocolName = "protocol"
        case instanceId = "instance_id"
        case port
        case capabilities
        case uiPath = "ui_path"
        case registeredAt = "registered_at"
    }

    init(
        protocolName: String = "treer.agent-interface/v1",
        instanceId: String = "",
        port: UInt16 = 0,
        capabilities: [String] = [],
        uiPath: String? = nil,
        registeredAt: Date? = nil
    ) {
        self.protocolName = protocolName
        self.instanceId = instanceId
        self.port = port
        self.capabilities = capabilities
        self.uiPath = uiPath
        self.registeredAt = registeredAt
    }

    func supports(_ capability: String) -> Bool {
        capabilities.contains(capability)
    }

    var hasUIPath: Bool {
        guard let uiPath, !uiPath.isEmpty else { return false }
        return true
    }
}

struct AgentInfo: Decodable, Equatable, Identifiable {
    var agentId: String
    var workspaceId: String?
    var serverId: String
    var kind: String
    var name: String
    var cwd: String
    var status: AgentStatus
    var pid: UInt32?
    var startedAt: Date?
    var updatedAt: Date
    var exitedAt: Date?
    var exitCode: Int32?
    var outputRevision: UInt64
    var interface: AgentInterface?

    var id: String { agentId }

    enum CodingKeys: String, CodingKey {
        case agentId = "agent_id"
        case workspaceId = "workspace_id"
        case serverId = "server_id"
        case kind
        case name
        case cwd
        case status
        case pid
        case startedAt = "started_at"
        case updatedAt = "updated_at"
        case exitedAt = "exited_at"
        case exitCode = "exit_code"
        case outputRevision = "output_revision"
        case interface
    }

    init(
        agentId: String,
        workspaceId: String? = nil,
        serverId: String,
        kind: String,
        name: String,
        cwd: String = ".",
        status: AgentStatus,
        pid: UInt32? = nil,
        startedAt: Date? = nil,
        updatedAt: Date = Date(),
        exitedAt: Date? = nil,
        exitCode: Int32? = nil,
        outputRevision: UInt64 = 0,
        interface: AgentInterface? = nil
    ) {
        self.agentId = agentId
        self.workspaceId = workspaceId
        self.serverId = serverId
        self.kind = kind
        self.name = name
        self.cwd = cwd
        self.status = status
        self.pid = pid
        self.startedAt = startedAt
        self.updatedAt = updatedAt
        self.exitedAt = exitedAt
        self.exitCode = exitCode
        self.outputRevision = outputRevision
        self.interface = interface
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        agentId = try container.decode(String.self, forKey: .agentId)
        workspaceId = try container.decodeIfPresent(String.self, forKey: .workspaceId)
        serverId = try container.decode(String.self, forKey: .serverId)
        kind = try container.decode(String.self, forKey: .kind)
        name = try container.decode(String.self, forKey: .name)
        cwd = try container.decodeIfPresent(String.self, forKey: .cwd) ?? "."
        status = try container.decodeIfPresent(AgentStatus.self, forKey: .status) ?? .unknown
        pid = try container.decodeIfPresent(UInt32.self, forKey: .pid)
        startedAt = try container.decodeIfPresent(Date.self, forKey: .startedAt)
        updatedAt = try container.decodeIfPresent(Date.self, forKey: .updatedAt) ?? Date()
        exitedAt = try container.decodeIfPresent(Date.self, forKey: .exitedAt)
        exitCode = try container.decodeIfPresent(Int32.self, forKey: .exitCode)
        outputRevision = try container.decodeIfPresent(UInt64.self, forKey: .outputRevision) ?? 0
        interface = try container.decodeIfPresent(AgentInterface.self, forKey: .interface)
    }

    func displayStatus(machine: ServerInfo?) -> String {
        if machine?.isOnline != true {
            return "offline"
        }
        return status.rawValue
    }

    var canPrompt: Bool {
        interface?.supports("prompt.submit") == true
    }

    var canReadTranscript: Bool {
        interface?.supports("transcript.read") == true
    }

    var canAbort: Bool {
        interface?.supports("abort") == true
    }

    var hasAgentUI: Bool {
        interface?.hasUIPath == true
    }
}

struct WorkspaceSnapshot: Decodable, Equatable {
    var revision: UInt64
    var workspace: Workspace
    var servers: [ServerInfo]
    var agents: [AgentInfo]

    func machine(for agent: AgentInfo) -> ServerInfo? {
        servers.first(where: { $0.serverId == agent.serverId })
    }

    func machine(id: String) -> ServerInfo? {
        servers.first(where: { $0.serverId == id })
    }
}

struct LaunchProfile: Decodable, Equatable, Identifiable {
    var profileId: String
    var workspaceId: String?
    var name: String
    var description: String
    var cwd: String
    var command: String
    var args: [String]

    var id: String { profileId }

    enum CodingKeys: String, CodingKey {
        case profileId = "profile_id"
        case workspaceId = "workspace_id"
        case name
        case description
        case cwd
        case command
        case args
    }

    init(
        profileId: String,
        workspaceId: String? = nil,
        name: String,
        description: String = "",
        cwd: String = ".",
        command: String,
        args: [String] = []
    ) {
        self.profileId = profileId
        self.workspaceId = workspaceId
        self.name = name
        self.description = description
        self.cwd = cwd
        self.command = command
        self.args = args
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        profileId = try container.decode(String.self, forKey: .profileId)
        workspaceId = try container.decodeIfPresent(String.self, forKey: .workspaceId)
        name = try container.decode(String.self, forKey: .name)
        description = try container.decodeIfPresent(String.self, forKey: .description) ?? ""
        cwd = try container.decodeIfPresent(String.self, forKey: .cwd) ?? "."
        command = try container.decode(String.self, forKey: .command)
        args = try container.decodeIfPresent([String].self, forKey: .args) ?? []
    }

    var inferredKind: String? {
        AgentCatalog.kind(fromCommand: command)
    }

    var looksAISCapable: Bool {
        inferredKind != nil
    }
}

struct AgentTranscriptEntry: Codable, Equatable, Identifiable {
    var id: String
    var kind: String
    var role: String?
    var content: JSONValue
    var createdAt: String?

    enum CodingKeys: String, CodingKey {
        case id
        case kind
        case role
        case content
        case createdAt = "created_at"
    }
}

struct AgentTranscriptResponse: Codable, Equatable {
    var agentId: String?
    var entries: [AgentTranscriptEntry]

    enum CodingKeys: String, CodingKey {
        case agentId = "agent_id"
        case entries
    }

    init(agentId: String? = nil, entries: [AgentTranscriptEntry]) {
        self.agentId = agentId
        self.entries = entries
    }

    var latestTurnText: String? {
        let turns = entries.filter { entry in
            let role = entry.role?.lowercased()
            return role == "user" || role == "assistant" || entry.kind == "message"
        }
        guard let last = turns.last ?? entries.last else { return nil }
        let text = last.content.flattenedText()
        return text.isEmpty ? nil : text
    }
}

struct OrganizationsResponse: Decodable {
    var organizations: [Organization]
}

struct WorkspacesResponse: Decodable {
    var workspaces: [Workspace]
}

struct LaunchProfilesResponse: Decodable {
    var profiles: [LaunchProfile]
}

struct WorkspaceEvent: Decodable {
    var revision: UInt64?
    var workspaceId: String?
    var event: String
    var data: WorkspaceSnapshot?

    enum CodingKeys: String, CodingKey {
        case revision
        case workspaceId = "workspace_id"
        case event
        case data
    }
}

struct APIErrorBody: Decodable {
    struct Envelope: Decodable {
        var message: String?
        var code: String?
    }

    var error: Envelope?
}

enum ConnectionState: String, Equatable {
    case idle
    case connecting
    case live
    case reconnecting
    case offline
}

enum AppTheme: String, CaseIterable, Identifiable {
    case system
    case light
    case dark

    var id: String { rawValue }

    var title: String {
        switch self {
        case .system: return "System"
        case .light: return "Light"
        case .dark: return "Dark"
        }
    }
}

struct CatalogEntry: Equatable, Identifiable {
    var kind: String
    var label: String
    var id: String { kind }
}

struct BootstrapInfo: Decodable, Equatable {
    var installCommand: String
    var connectCommand: String
    var enrollmentKey: String
    var scriptUrl: String
    var workspaceId: String

    enum CodingKeys: String, CodingKey {
        case installCommand = "install_command"
        case connectCommand = "connect_command"
        case enrollmentKey = "enrollment_key"
        case scriptUrl = "script_url"
        case workspaceId = "workspace_id"
    }

    static let fixture = BootstrapInfo(
        installCommand: "curl -fsSL 'http://127.0.0.1:8787/install.sh' | sh",
        connectCommand: "TREER_ENROLLMENT_KEY='enr_v1_fixture' treer-agent-server connect --proxy 'http://127.0.0.1:8787/'",
        enrollmentKey: "enr_v1_fixture",
        scriptUrl: "http://127.0.0.1:8787/install.sh",
        workspaceId: "ws_fixture"
    )
}

enum AgentCatalog {
    static let entries: [CatalogEntry] = [
        CatalogEntry(kind: "claude", label: "Claude"),
        CatalogEntry(kind: "cursor", label: "Cursor"),
        CatalogEntry(kind: "grok", label: "Grok"),
        CatalogEntry(kind: "opencode", label: "OpenCode"),
        CatalogEntry(kind: "pi", label: "Pi"),
        CatalogEntry(kind: "codex", label: "Codex"),
    ]

    static let kinds = entries.map(\.kind)

    static func label(for kind: String) -> String {
        entries.first { $0.kind == kind }?.label ?? kind
    }

    static func wireKind(_ kind: String) -> String {
        switch kind {
        case "terminal":
            return "command"
        case "cursor-agent":
            return "cursor"
        default:
            return kind
        }
    }

    static func preferredKind(on machine: ServerInfo?) -> String {
        if machine?.availableAgents?.contains("codex") == true {
            return "codex"
        }
        return "terminal"
    }

    static func kind(fromCommand command: String) -> String? {
        let file = command.split(whereSeparator: { $0 == "/" || $0 == "\\" }).last.map(String.init) ?? command
        let name = file.hasSuffix(".exe") ? String(file.dropLast(4)) : file
        if name == "cursor-agent" { return "cursor" }
        if kinds.contains(name) { return name }
        return nil
    }

    static func isInstalled(kind: String, on machine: ServerInfo?) -> Bool? {
        guard let available = machine?.availableAgents else { return nil }
        let normalized = kind == "cursor-agent" ? "cursor" : kind
        return available.contains(normalized)
    }
}

enum ObjectIDSuffix {
    static func suffix(for raw: String) -> String {
        var value = raw
        if value.hasPrefix("ag_") {
            value.removeFirst(3)
        } else if value.hasPrefix("srv_") {
            value.removeFirst(4)
        } else if value.hasPrefix("lp_") {
            value.removeFirst(3)
        }
        return String(value.suffix(6))
    }
}

enum AgentNaming {
    static func defaultName(kind: String, now: Date = Date()) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM-dd"
        let prefix: String
        switch kind {
        case "terminal", "command":
            prefix = "terminal"
        case "codex", "claude", "installer":
            prefix = kind
        default:
            prefix = "agent"
        }
        return "\(prefix)-\(formatter.string(from: now))"
    }

    static func defaultProfileName(_ profileName: String, now: Date = Date()) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM-dd-HHmmss"
        let slug = profileName
            .lowercased()
            .replacingOccurrences(of: "[^a-z0-9]+", with: "-", options: .regularExpression)
            .trimmingCharacters(in: CharacterSet(charactersIn: "-"))
        let trimmed = String((slug.isEmpty ? "agent" : slug).prefix(40))
        return String("\(trimmed)-\(formatter.string(from: now))".prefix(80))
    }
}

enum MachineRecovery {
    static func restartController(workspaceId: String) -> String {
        "treer-agent-server service --workspace \(workspaceId) restart-controller"
    }

    static func start(workspaceId: String) -> String {
        "treer-agent-server service --workspace \(workspaceId) start"
    }
}

enum PromptPolicy {
    static let confirmLength = 500

    static func requiresConfirmation(text: String, agentStatus: AgentStatus) -> Bool {
        if text.count > confirmLength { return true }
        return agentStatus == .working || agentStatus == .starting
    }
}
