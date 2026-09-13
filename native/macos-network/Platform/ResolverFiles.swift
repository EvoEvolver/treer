import Foundation
import CryptoKit
import TreerNetworkCore

/// Own exact-domain supplemental resolver files, never the global DNS settings.
/// Refuse to replace a file unless its complete contents match our last write.
final class ResolverFiles {
    private let directory: URL
    private var owned: [URL: Data] = [:]
    init(directory: URL = URL(fileURLWithPath: "/etc/resolver")) { self.directory = directory }

    func replace(hosts: [String], port: UInt16) throws {
        guard port > 0 else { throw NativeNetworkError.invalidArguments }
        let names = try hosts.map(VirtualDNS.normalize)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
                                                attributes: [.posixPermissions: 0o755])
        var wanted: [URL: Data] = [:]
        for name in names {
            let digest = SHA256.hash(data: Data(name.utf8)).map { String(format: "%02x", $0) }.joined()
            let path = directory.appendingPathComponent("treer-" + digest)
            let data = Data("# Treer owned virtual DNS v1\ndomain \(name)\nnameserver 127.0.0.1\nport \(port)\ntimeout 1\n".utf8)
            if FileManager.default.fileExists(atPath: path.path) {
                let attributes = try FileManager.default.attributesOfItem(atPath: path.path)
                guard attributes[.type] as? FileAttributeType == .typeRegular else { throw CocoaError(.fileWriteNoPermission) }
                let existing = try Data(contentsOf: path)
                // Permit restart recovery only of our exact fixed template with
                // a numeric prior port; arbitrary user content is never replaced.
                let validPrior = (1...65535).contains(Self.priorPort(existing, name: name) ?? 0)
                guard existing == owned[path] || validPrior else { throw CocoaError(.fileWriteNoPermission) }
            }
            if (try? Data(contentsOf: path)) != data { try data.write(to: path, options: .atomic) }
            owned[path] = data
            wanted[path] = data
        }
        for (path, previous) in owned where wanted[path] == nil {
            if (try? Data(contentsOf: path)) == previous { try FileManager.default.removeItem(at: path) }
        }
        owned = wanted
    }

    private static func priorPort(_ data: Data, name: String) -> Int? {
        guard let text = String(data: data, encoding: .utf8) else { return nil }
        let lines = text.split(separator: "\n")
        guard lines.count == 5, lines[0] == "# Treer owned virtual DNS v1",
              lines[1] == "domain \(name)", lines[2] == "nameserver 127.0.0.1",
              lines[3].hasPrefix("port "), lines[4] == "timeout 1" else { return nil }
        return Int(lines[3].dropFirst(5))
    }
}
