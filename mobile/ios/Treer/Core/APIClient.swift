import Foundation
import UIKit

struct APIError: LocalizedError, Equatable {
    var status: Int
    var message: String
    var code: String?

    var errorDescription: String? { message }

    var isUnauthorized: Bool { status == 401 }
}

final class APIClient {
    var proxyURL: URL
    var token: String?
    var session: URLSession

    static let clientHeader = "X-Treer-Client"
    static let clientValue = "mobile_ios"

    init(proxyURL: URL, token: String? = nil, session: URLSession = .shared) {
        self.proxyURL = proxyURL
        self.token = token
        self.session = session
    }

    func health() async throws -> HealthStatus {
        try await get("/api/health")
    }

    func authConfig() async throws -> AuthConfig {
        try await get("/api/auth/config")
    }

    func login(email: String, password: String, deviceId: String, deviceName: String) async throws -> SessionResponse {
        try await send(
            "/api/auth/login",
            method: "POST",
            body: [
                "email": email,
                "password": password,
                "device_id": deviceId,
                "device_name": deviceName
            ],
            authorized: false,
            nativeAuth: true
        )
    }

    func register(
        email: String,
        password: String,
        preferredName: String,
        invite: String?,
        deviceId: String,
        deviceName: String
    ) async throws -> SessionResponse {
        var body: [String: Any] = [
            "email": email,
            "password": password,
            "preferred_name": preferredName,
            "device_id": deviceId,
            "device_name": deviceName
        ]
        if let invite, !invite.isEmpty {
            body["invite"] = invite
        }
        return try await send(
            "/api/auth/register",
            method: "POST",
            body: body,
            authorized: false,
            nativeAuth: true
        )
    }

    func requestPasswordReset(email: String) async throws {
        let _: [String: Bool] = try await send(
            "/api/auth/request-password-reset",
            method: "POST",
            body: ["email": email],
            authorized: false
        )
    }

    func resetPassword(token: String, password: String) async throws {
        let _: [String: Bool] = try await send(
            "/api/auth/reset-password",
            method: "POST",
            body: ["token": token, "password": password],
            authorized: false
        )
    }

    func me() async throws -> UserProfile {
        try await get("/api/auth/me")
    }

    func updateProfile(email: String, preferredName: String) async throws -> UserProfile {
        try await send(
            "/api/auth/profile",
            method: "PATCH",
            body: ["email": email, "preferred_name": preferredName]
        )
    }

    func logout() async throws {
        do {
            let _: [String: Bool] = try await send("/api/auth/logout", method: "POST", body: [:])
        } catch let error as APIError where error.isUnauthorized {
            return
        }
    }

    func organizations() async throws -> [Organization] {
        let response: OrganizationsResponse = try await get("/api/organizations")
        return response.organizations
    }

    func workspaces(organizationId: String) async throws -> [Workspace] {
        let encoded = organizationId.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? organizationId
        let response: WorkspacesResponse = try await get("/api/workspaces?organization_id=\(encoded)")
        return response.workspaces
    }

    func createWorkspace(organizationId: String, name: String) async throws -> Workspace {
        struct Envelope: Decodable { var workspace: Workspace }
        let envelope: Envelope = try await send(
            "/api/workspaces",
            method: "POST",
            body: ["organization_id": organizationId, "name": name]
        )
        return envelope.workspace
    }

    func snapshot(workspaceId: String) async throws -> WorkspaceSnapshot {
        try await get("/api/workspaces/\(encode(workspaceId))/snapshot")
    }

    func bootstrap(workspaceId: String) async throws -> BootstrapInfo {
        try await send(
            "/api/workspaces/\(encode(workspaceId))/bootstrap",
            method: "POST",
            body: [:]
        )
    }

    func launchProfiles(workspaceId: String) async throws -> [LaunchProfile] {
        let response: LaunchProfilesResponse = try await get("/api/workspaces/\(encode(workspaceId))/launch-profiles")
        return response.profiles
    }

    func createAgent(
        workspaceId: String,
        serverId: String,
        kind: String,
        name: String,
        cwd: String = ".",
        args: [String] = []
    ) async throws -> AgentInfo {
        try await send(
            "/api/workspaces/\(encode(workspaceId))/agents",
            method: "POST",
            body: [
                "server_id": serverId,
                "kind": kind,
                "name": name,
                "cwd": cwd,
                "args": args,
                "cols": 120,
                "rows": 36
            ]
        )
    }

    func launchProfile(
        workspaceId: String,
        profileId: String,
        serverId: String,
        agentName: String
    ) async throws -> AgentInfo {
        try await send(
            "/api/workspaces/\(encode(workspaceId))/launch-profiles/\(encode(profileId))/launch",
            method: "POST",
            body: [
                "server_id": serverId,
                "agent_name": agentName,
                "cols": 120,
                "rows": 36
            ]
        )
    }

    func agent(workspaceId: String, agentId: String) async throws -> AgentInfo {
        try await get("/api/workspaces/\(encode(workspaceId))/agents/\(encode(agentId))")
    }

    func prompt(workspaceId: String, agentId: String, text: String) async throws {
        let _: AgentInfo = try await send(
            "/api/workspaces/\(encode(workspaceId))/agents/\(encode(agentId))/prompt",
            method: "POST",
            body: ["text": text]
        )
    }

    func transcript(workspaceId: String, agentId: String) async throws -> AgentTranscriptResponse {
        try await get("/api/workspaces/\(encode(workspaceId))/agents/\(encode(agentId))/transcript")
    }

    func stop(workspaceId: String, agentId: String) async throws {
        let _: AgentInfo = try await send(
            "/api/workspaces/\(encode(workspaceId))/agents/\(encode(agentId))/stop",
            method: "POST",
            body: [:]
        )
    }

    func abort(workspaceId: String, agentId: String) async throws {
        let _: AgentInfo = try await send(
            "/api/workspaces/\(encode(workspaceId))/agents/\(encode(agentId))/abort",
            method: "POST",
            body: [:]
        )
    }

    func deleteAgent(workspaceId: String, agentId: String) async throws {
        try await sendEmpty("/api/workspaces/\(encode(workspaceId))/agents/\(encode(agentId))", method: "DELETE")
    }

    func renameAgent(workspaceId: String, agentId: String, name: String) async throws -> AgentInfo {
        try await send(
            "/api/workspaces/\(encode(workspaceId))/agents/\(encode(agentId))",
            method: "PATCH",
            body: ["name": name]
        )
    }

    func eventsWebSocketRequest(workspaceId: String) throws -> URLRequest {
        try websocketRequest("/api/workspaces/\(encode(workspaceId))/events")
    }

    func voiceAsrStatus(workspaceId: String) async throws -> VoiceAsrStatus {
        try await get("/api/workspaces/\(encode(workspaceId))/voice/asr")
    }

    func voiceAsrWebSocketRequest(workspaceId: String) throws -> URLRequest {
        try websocketRequest("/api/workspaces/\(encode(workspaceId))/voice/asr/stream")
    }

    func voiceCommandStatus(workspaceId: String) async throws -> VoiceCommandStatus {
        try await get("/api/workspaces/\(encode(workspaceId))/voice/command")
    }

    func voiceCommand(workspaceId: String, text: String, history: [VoiceLine] = []) async throws -> VoiceCommandReply {
        let turns: [[String: String]] = history.suffix(12).compactMap { line in
            guard line.role == "user" || line.role == "assistant", !line.text.isEmpty else { return nil }
            return ["role": line.role, "text": line.text]
        }
        var body: [String: Any] = ["text": text]
        body["history"] = turns
        return try await send(
            "/api/workspaces/\(encode(workspaceId))/voice/command",
            method: "POST",
            body: body
        )
    }

    func terminalWebSocketRequest(workspaceId: String, agentId: String, cols: Int, rows: Int) throws -> URLRequest {
        try websocketRequest("/api/workspaces/\(encode(workspaceId))/agents/\(encode(agentId))/terminal?cols=\(cols)&rows=\(rows)")
    }

    func interfaceURL(workspaceId: String, agentId: String, relative: String) -> URL {
        let trimmed = relative.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        let path = "/api/workspaces/\(encode(workspaceId))/agents/\(encode(agentId))/interface/ui/\(trimmed)"
        return url(path)
    }

    func authorizedRequest(_ url: URL, method: String = "GET") -> URLRequest {
        var request = URLRequest(url: url)
        request.httpMethod = method
        applyAuth(to: &request, nativeAuth: false)
        return request
    }

    func probeProxy() async throws {
        do {
            let health = try await health()
            if health.service == "treer-proxy" || health.status == "ok" {
                return
            }
        } catch {
            // Fall through to auth config.
        }
        _ = try await authConfig()
    }

    private func get<T: Decodable>(_ path: String) async throws -> T {
        try await send(path, method: "GET", body: nil as [String: Any]?, authorized: true)
    }

    private func sendEmpty(_ path: String, method: String) async throws {
        var request = URLRequest(url: url(path))
        request.httpMethod = method
        applyAuth(to: &request, nativeAuth: false)
        let (data, response) = try await session.data(for: request)
        try throwIfNeeded(data: data, response: response)
    }

    private func send<T: Decodable>(
        _ path: String,
        method: String,
        body: [String: Any]?,
        authorized: Bool = true,
        nativeAuth: Bool = false
    ) async throws -> T {
        var request = URLRequest(url: url(path))
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        applyAuth(to: &request, nativeAuth: nativeAuth, authorized: authorized)
        if let body {
            request.httpBody = try JSONSerialization.data(withJSONObject: body, options: [])
        }
        let (data, response) = try await session.data(for: request)
        try throwIfNeeded(data: data, response: response)
        if T.self == SessionResponse.self || T.self == UserProfile.self || T.self == AgentInfo.self
            || T.self == WorkspaceSnapshot.self || T.self == AgentTranscriptResponse.self
            || T.self == AuthConfig.self || T.self == HealthStatus.self
            || T.self == OrganizationsResponse.self || T.self == WorkspacesResponse.self
            || T.self == LaunchProfilesResponse.self
            || T.self == VoiceAsrStatus.self || T.self == VoiceCommandStatus.self
            || T.self == VoiceCommandReply.self || T.self == BootstrapInfo.self
        {
            return try TreerJSON.decoder.decode(T.self, from: data)
        }
        do {
            return try TreerJSON.decoder.decode(T.self, from: data)
        } catch {
            if let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
               object["ok"] as? Bool == true,
               T.self == [String: Bool].self
            {
                return ["ok": true] as! T
            }
            throw error
        }
    }

    private func throwIfNeeded(data: Data, response: URLResponse) throws {
        guard let http = response as? HTTPURLResponse else {
            throw APIError(status: -1, message: "Invalid response", code: nil)
        }
        guard (200 ..< 300).contains(http.statusCode) else {
            let body = try? TreerJSON.decoder.decode(APIErrorBody.self, from: data)
            let message = body?.error?.message ?? String(data: data, encoding: .utf8) ?? "HTTP \(http.statusCode)"
            throw APIError(status: http.statusCode, message: message, code: body?.error?.code)
        }
    }

    private func applyAuth(to request: inout URLRequest, nativeAuth: Bool, authorized: Bool = true) {
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if nativeAuth {
            request.setValue(Self.clientValue, forHTTPHeaderField: Self.clientHeader)
        }
        if authorized, let token, !token.isEmpty {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
    }

    private func websocketRequest(_ path: String) throws -> URLRequest {
        var components = URLComponents(url: url(path), resolvingAgainstBaseURL: false)
        if components?.scheme == "https" {
            components?.scheme = "wss"
        } else if components?.scheme == "http" {
            components?.scheme = "ws"
        }
        guard let socketURL = components?.url else {
            throw APIError(status: -1, message: "Invalid WebSocket URL", code: nil)
        }
        var request = URLRequest(url: socketURL)
        applyAuth(to: &request, nativeAuth: false)
        return request
    }

    func url(_ path: String) -> URL {
        let root = proxyURL.absoluteString.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        let suffix = path.hasPrefix("/") ? path : "/\(path)"
        return URL(string: root + suffix)!
    }

    private func encode(_ value: String) -> String {
        value.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? value
    }

    static func normalizeProxyURL(_ raw: String) throws -> URL {
        var trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.hasSuffix("/") {
            trimmed.removeLast()
        }
        if !trimmed.contains("://") {
            trimmed = "http://\(trimmed)"
        }
        guard let url = URL(string: trimmed), let scheme = url.scheme, ["http", "https"].contains(scheme), url.host != nil else {
            throw APIError(status: -1, message: "Enter a Treer Proxy URL such as http://192.168.1.10:8080", code: "invalid_proxy_url")
        }
        return url
    }

    static func deviceName() -> String {
        UIDevice.current.name
    }
}
