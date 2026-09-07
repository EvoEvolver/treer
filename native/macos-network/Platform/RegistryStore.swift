import Darwin
import Foundation

/// Stored in the provider's own Application Support directory. This is not a
/// shared App Group: workloads must not be able to forge captured identities.
final class RegistryStore {
    private let directory: URL
    private let file: URL
    init(directory: URL, fileName: String = "processes-v1.json") {
        self.directory = directory
        self.file = directory.appendingPathComponent(fileName)
    }
    static func providerStore(fileName: String = "processes-v1.json") throws -> RegistryStore {
        let support = try FileManager.default.url(for: .applicationSupportDirectory,
            in: .userDomainMask, appropriateFor: nil, create: true)
        return RegistryStore(directory: support.appendingPathComponent("org.treer.network.extension", isDirectory: true), fileName: fileName)
    }
    private func prepare() throws {
        let manager = FileManager.default
        try manager.createDirectory(at: directory, withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700])
        let attributes = try manager.attributesOfItem(atPath: directory.path)
        guard attributes[.type] as? FileAttributeType == .typeDirectory,
              (attributes[.ownerAccountID] as? NSNumber)?.uint32Value == getuid(),
              (attributes[.posixPermissions] as? NSNumber)?.intValue == 0o700 else {
            throw CocoaError(.fileReadNoPermission)
        }
    }
    func load() throws -> Data? {
        try prepare()
        do {
            let attributes = try FileManager.default.attributesOfItem(atPath: file.path)
            guard attributes[.type] as? FileAttributeType == .typeRegular,
                  (attributes[.ownerAccountID] as? NSNumber)?.uint32Value == getuid(),
                  (attributes[.posixPermissions] as? NSNumber)?.intValue == 0o600 else {
                throw CocoaError(.fileReadNoPermission)
            }
            return try Data(contentsOf: file)
        } catch let error as CocoaError where error.code == .fileReadNoSuchFile {
            return nil
        }
    }
    func save(_ data: Data) throws {
        try prepare()
        // The enclosing 0700 directory protects the atomic temporary file too.
        try data.write(to: file, options: .atomic)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: file.path)
    }
}
