import Darwin
import Foundation
import NetworkExtension
import SystemExtensions
import TreerNetworkCore

private let extensionID = "org.treer.network.extension"

@main
struct TreerNetworkApp {
    static func main() async {
        do { try await run(Array(CommandLine.arguments.dropFirst())) }
        catch {
            FileHandle.standardError.write(Data("Treer network: \(error)\n".utf8))
            exit(1)
        }
    }

    static func run(_ args: [String]) async throws {
        guard let command = args.first else { throw NativeNetworkError.invalidArguments }
        switch command {
        case "activate":
            try await ExtensionActivation.run()
            let manager = try await ownManager() ?? NETransparentProxyManager()
            let configuration = NETunnelProviderProtocol()
            configuration.providerBundleIdentifier = extensionID
            configuration.serverAddress = "127.0.0.1"
            manager.protocolConfiguration = configuration
            manager.localizedDescription = "Treer Agent Network"
            manager.isEnabled = true
            try await manager.saveToPreferences()
            try await manager.loadFromPreferences()
            try manager.connection.startVPNTunnel()
            // startVPNTunnel only starts activation. Status/registration require
            // a provider reply after settings are applied, never a timed sleep.
            print("Treer extension activation requested. Use status to check readiness.")
        case "deactivate":
            if let manager = try await ownManager() {
                manager.connection.stopVPNTunnel()
                manager.isEnabled = false
                try await manager.saveToPreferences()
            }
        case "uninstall":
            if let manager = try await ownManager() {
                manager.connection.stopVPNTunnel()
                try await manager.removeFromPreferences()
            }
            try await ExtensionActivation.run(activate: false)
            print("Treer system extension deactivation completed. The app bundle can now be removed.")
        case "status":
            let reply = try await message(Data(#"{"version":1,"operation":"status"}"#.utf8))
            FileHandle.standardOutput.write(try JSONEncoder().encode(reply) + Data([10]))
            if !reply.ready { throw NativeNetworkError.unavailable }
        case "sync-hosts":
            guard args.count == 4, args[1] == "--network-proxy", args[3] == "--hosts-stdin",
                  let url = URLComponents(string: args[2]), url.scheme == "socks5h",
                  url.host == "127.0.0.1", let number = url.port, let port = UInt16(exactly: number),
                  let process = ProcessInstance.read(getppid()) else { throw NativeNetworkError.invalidArguments }
            let data = FileHandle.standardInput.readDataToEndOfFile()
            guard data.count <= 4 * 1024 * 1024 else { throw NativeNetworkError.invalidArguments }
            let names = try JSONDecoder().decode([String].self, from: data)
            let controller = CaptureRegistration(pid: process.pid, startSeconds: process.seconds,
                startMicroseconds: process.microseconds, agentID: "controller", socksPort: port)
            let reply = try await message(JSONEncoder().encode(CaptureHostSnapshot(controller: controller, hosts: names)))
            guard reply.ready else { throw NativeNetworkError.rejected }
            FileHandle.standardOutput.write(try JSONEncoder().encode(reply) + Data([10]))
        case "exec":
            guard args.count >= 7, args[1] == "--agent-id", args[3] == "--network-proxy",
                  let url = URLComponents(string: args[4]), url.scheme == "socks5h",
                  url.host == "127.0.0.1", let number = url.port, let port = UInt16(exactly: number),
                  args[5] == "--",
                  let process = ProcessInstance.read(getpid()) else { throw NativeNetworkError.invalidArguments }
            let registration = CaptureRegistration(pid: process.pid, startSeconds: process.seconds,
                startMicroseconds: process.microseconds, agentID: args[2], socksPort: port)
            guard registration.valid else { throw NativeNetworkError.invalidArguments }
            let reply = try await message(JSONEncoder().encode(registration))
            guard reply.version == 1, reply.ready else { throw NativeNetworkError.rejected }
            // exec preserves the registered PID and birth time; application
            // instructions cannot execute before the extension acknowledges it.
            let command = Array(args.dropFirst(6))
            let pointers = command.map { strdup($0) } + [nil]
            defer { pointers.forEach { free($0) } }
            pointers.withUnsafeBufferPointer { buffer in _ = execvp(command[0], buffer.baseAddress!) }
            throw NSError(domain: NSPOSIXErrorDomain, code: Int(errno))
        default: throw NativeNetworkError.invalidArguments
        }
    }

    static func ownManager() async throws -> NETransparentProxyManager? {
        try await NETransparentProxyManager.loadAllFromPreferences().first {
            ($0.protocolConfiguration as? NETunnelProviderProtocol)?.providerBundleIdentifier == extensionID
        }
    }

    static func message(_ data: Data) async throws -> CaptureReply {
        guard let manager = try await ownManager(), manager.isEnabled,
              manager.connection.status == .connected,
              let session = manager.connection as? NETunnelProviderSession else {
            throw NativeNetworkError.unavailable
        }
        let response: Data = try await withCheckedThrowingContinuation { continuation in
            let gate = MessageReply(continuation)
            do {
                try session.sendProviderMessage(data) { response in
                    if let response { gate.finish(.success(response)) }
                    else { gate.finish(.failure(NativeNetworkError.unavailable)) }
                }
                DispatchQueue.global().asyncAfter(deadline: .now() + 10) {
                    gate.finish(.failure(NativeNetworkError.timeout))
                }
            } catch { gate.finish(.failure(error)) }
        }
        let reply = try JSONDecoder().decode(CaptureReply.self, from: response)
        guard reply.version == 1 else { throw NativeNetworkError.malformedReply }
        return reply
    }
}

private final class MessageReply: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<Data, Error>?
    init(_ continuation: CheckedContinuation<Data, Error>) { self.continuation = continuation }
    func finish(_ result: Result<Data, Error>) {
        lock.lock()
        let callback = continuation
        continuation = nil
        lock.unlock()
        callback?.resume(with: result)
    }
}

private final class ExtensionActivation: NSObject, OSSystemExtensionRequestDelegate {
    private var continuation: CheckedContinuation<Void, Error>?
    static func run(activate: Bool = true) async throws {
        let installer = ExtensionActivation()
        try await withCheckedThrowingContinuation { continuation in
            installer.continuation = continuation
            let request = activate
                ? OSSystemExtensionRequest.activationRequest(forExtensionWithIdentifier: extensionID, queue: .main)
                : OSSystemExtensionRequest.deactivationRequest(forExtensionWithIdentifier: extensionID, queue: .main)
            request.delegate = installer
            OSSystemExtensionManager.shared.submitRequest(request)
        }
        withExtendedLifetime(installer) {}
    }
    func requestNeedsUserApproval(_ request: OSSystemExtensionRequest) {
        FileHandle.standardError.write(Data("Approve Treer Agent Network in System Settings to finish activation.\n".utf8))
    }
    func request(_ request: OSSystemExtensionRequest, actionForReplacingExtension existing: OSSystemExtensionProperties,
                 withExtension ext: OSSystemExtensionProperties) -> OSSystemExtensionRequest.ReplacementAction { .replace }
    func request(_ request: OSSystemExtensionRequest, didFailWithError error: Error) {
        continuation?.resume(throwing: error); continuation = nil
    }
    func request(_ request: OSSystemExtensionRequest, didFinishWithResult result: OSSystemExtensionRequest.Result) {
        if result == .completed { continuation?.resume() }
        else { continuation?.resume(throwing: NativeNetworkError.unavailable) }
        continuation = nil
    }
}
