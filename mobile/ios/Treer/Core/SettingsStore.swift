import Foundation

struct AppSettingsStore {
    private let defaults: UserDefaults
    private let tokenStore: TokenStore

    static let tokenAccount = "session"
    static let deviceAccount = "device-id"

    init(defaults: UserDefaults = .standard, tokenStore: TokenStore = FallbackTokenStore()) {
        self.defaults = defaults
        self.tokenStore = tokenStore
    }

    var proxyURLString: String? {
        get { defaults.string(forKey: Keys.proxyURL) }
        nonmutating set { defaults.set(newValue, forKey: Keys.proxyURL) }
    }

    var lastOrganizationId: String? {
        get { defaults.string(forKey: Keys.orgId) }
        nonmutating set { defaults.set(newValue, forKey: Keys.orgId) }
    }

    var lastWorkspaceId: String? {
        get { defaults.string(forKey: Keys.workspaceId) }
        nonmutating set { defaults.set(newValue, forKey: Keys.workspaceId) }
    }

    var theme: AppTheme {
        get { AppTheme(rawValue: defaults.string(forKey: Keys.theme) ?? "") ?? .system }
        nonmutating set { defaults.set(newValue.rawValue, forKey: Keys.theme) }
    }

    var showTerminalControls: Bool {
        get { defaults.bool(forKey: Keys.showTerminal) }
        nonmutating set { defaults.set(newValue, forKey: Keys.showTerminal) }
    }

    func sessionToken() throws -> String? {
        try tokenStore.readToken(account: Self.tokenAccount)
    }

    func saveSessionToken(_ token: String) throws {
        try tokenStore.writeToken(token, account: Self.tokenAccount)
    }

    func clearSessionToken() throws {
        try tokenStore.deleteToken(account: Self.tokenAccount)
    }

    func deviceId() -> String {
        if let existing = defaults.string(forKey: Keys.deviceId), UUID(uuidString: existing) != nil {
            return existing
        }
        if let stored = try? tokenStore.readToken(account: Self.deviceAccount), UUID(uuidString: stored) != nil {
            defaults.set(stored, forKey: Keys.deviceId)
            return stored
        }
        let created = UUID().uuidString
        defaults.set(created, forKey: Keys.deviceId)
        try? tokenStore.writeToken(created, account: Self.deviceAccount)
        return created
    }

    var deviceName: String {
        #if os(iOS)
        return ProcessInfo.processInfo.hostName
        #else
        return "Treer iOS"
        #endif
    }

    func reset() {
        defaults.removeObject(forKey: Keys.proxyURL)
        defaults.removeObject(forKey: Keys.orgId)
        defaults.removeObject(forKey: Keys.workspaceId)
        defaults.removeObject(forKey: Keys.theme)
        defaults.removeObject(forKey: Keys.showTerminal)
        try? clearSessionToken()
    }

    private enum Keys {
        static let proxyURL = "treer.proxyURL"
        static let orgId = "treer.lastOrganizationId"
        static let workspaceId = "treer.lastWorkspaceId"
        static let theme = "treer.theme"
        static let showTerminal = "treer.showTerminalControls"
        static let deviceId = "treer.deviceId"
    }
}
