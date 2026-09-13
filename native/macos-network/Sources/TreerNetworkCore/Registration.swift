import Foundation

/// Deliberately versioned JSON at the native OS boundary. No Host protocol or
/// workload secrets are persisted by the extension.
public struct CaptureRegistration: Codable, Equatable {
    public var version: Int
    public var pid: Int32
    public var startSeconds: UInt64
    public var startMicroseconds: UInt64
    public var agentID: String
    public var socksPort: UInt16
    public init(pid: Int32, startSeconds: UInt64, startMicroseconds: UInt64,
                agentID: String, socksPort: UInt16) {
        version = 1
        self.pid = pid
        self.startSeconds = startSeconds
        self.startMicroseconds = startMicroseconds
        self.agentID = agentID
        self.socksPort = socksPort
    }
    public var valid: Bool {
        version == 1 && pid > 1 && startSeconds > 0 && socksPort > 0
            && !agentID.isEmpty && agentID.utf8.count <= 255
            && agentID.unicodeScalars.allSatisfy { $0.isASCII && $0.value > 32 && $0.value < 127 }
    }
}

public struct CaptureReply: Codable {
    public var version: Int = 1
    public var ready: Bool
    public var error: String?
    public var capabilities: [String: String]?
    public init(ready: Bool, error: String? = nil) {
        self.ready = ready
        self.error = error
        self.capabilities = nil
    }
}

public struct CaptureHostSnapshot: Codable {
    public var version = 1
    public var operation = "hosts"
    public var controller: CaptureRegistration
    public var hosts: [String]
    public init(controller: CaptureRegistration, hosts: [String]) {
        self.controller = controller; self.hosts = hosts
    }
}
