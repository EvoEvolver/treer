import Darwin
import Foundation
import TreerNetworkCore

struct ProcessInstance: Equatable, Codable {
    let pid: Int32
    let parent: Int32
    let seconds: UInt64
    let microseconds: UInt64

    static func read(_ pid: Int32) -> ProcessInstance? {
        var info = proc_bsdinfo()
        let size = Int32(MemoryLayout<proc_bsdinfo>.size)
        guard proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, size) == size else { return nil }
        return ProcessInstance(pid: pid, parent: Int32(info.pbi_ppid),
                               seconds: info.pbi_start_tvsec, microseconds: info.pbi_start_tvusec)
    }
    func matches(_ registration: CaptureRegistration) -> Bool {
        pid == registration.pid && seconds == registration.startSeconds
            && microseconds == registration.startMicroseconds
    }
    func sameBirth(as other: ProcessInstance) -> Bool {
        pid == other.pid && seconds == other.seconds && microseconds == other.microseconds
    }
}

/// Accessed under the provider's lock. Keys include birth time, so a reused PID
/// never inherits an old Agent registration. Descendants are resolved at flow
/// time and retained after their first observed flow, including after reparenting.
/// A child detached before its first flow is not covered by ancestry lookup.
final class ProcessRegistry {
    private var roots: [Int32: CaptureRegistration] = [:]
    private var observed: [Int32: (ProcessInstance, CaptureRegistration)] = [:]

    private struct Entry: Codable {
        let process: ProcessInstance
        let registration: CaptureRegistration
    }
    private struct Snapshot: Codable {
        let version: Int
        let boot: String
        let roots: [CaptureRegistration]
        let observed: [Entry]
    }

    static func bootSession() throws -> String {
        var size = 0
        guard sysctlbyname("kern.bootsessionuuid", nil, &size, nil, 0) == 0 else {
            throw POSIXError(.EIO)
        }
        var bytes = [CChar](repeating: 0, count: size)
        guard sysctlbyname("kern.bootsessionuuid", &bytes, &size, nil, 0) == 0 else {
            throw POSIXError(.EIO)
        }
        return String(cString: bytes)
    }

    func checkpoint(boot: String) throws -> Data {
        prune()
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return try encoder.encode(Snapshot(version: 1, boot: boot,
            roots: roots.values.sorted { $0.pid < $1.pid }, observed: observed.values.sorted { $0.0.pid < $1.0.pid }.map {
                Entry(process: $0.0, registration: $0.1)
            }))
    }

    /// A checkpoint is private provider state, never an App-supplied message.
    /// Validate all entries before publishing the restored registry.
    func restore(_ data: Data, boot: String) throws {
        let snapshot = try JSONDecoder().decode(Snapshot.self, from: data)
        guard snapshot.version == 1, snapshot.boot == boot else { return }
        var restoredRoots: [Int32: CaptureRegistration] = [:]
        var restoredObserved: [Int32: (ProcessInstance, CaptureRegistration)] = [:]
        for registration in snapshot.roots where registration.valid {
            if ProcessInstance.read(registration.pid)?.matches(registration) == true {
                restoredRoots[registration.pid] = registration
            }
        }
        // An observed child remains managed even if its original parent exited.
        for entry in snapshot.observed where entry.registration.valid {
            if ProcessInstance.read(entry.process.pid)?.sameBirth(as: entry.process) == true {
                restoredObserved[entry.process.pid] = (entry.process, entry.registration)
            }
        }
        roots = restoredRoots
        observed = restoredObserved
    }

    func register(_ registration: CaptureRegistration) -> Bool {
        guard registration.valid, ProcessInstance.read(registration.pid)?.matches(registration) == true
        else { return false }
        if let existing = roots[registration.pid], existing != registration,
           ProcessInstance.read(registration.pid)?.matches(existing) == true { return false }
        roots[registration.pid] = registration
        prune()
        return true
    }

    func lookup(_ tokenData: Data?) -> CaptureRegistration? {
        guard let tokenData, tokenData.count == MemoryLayout<audit_token_t>.size else { return nil }
        var token = audit_token_t()
        _ = withUnsafeMutableBytes(of: &token) { tokenData.copyBytes(to: $0) }
        let pid = audit_token_to_pid(token)
        guard let instance = ProcessInstance.read(pid) else { return nil }
        // libproc validates the audit token's PID version. A delayed flow from
        // a dead process must not be attributed to a new process with its PID.
        var path = [UInt8](repeating: 0, count: 4096)
        guard proc_pidpath_audittoken(&token, &path, UInt32(path.count)) > 0 else { return nil }
        if let (prior, registration) = observed[pid], prior.sameBirth(as: instance) { return registration }
        var current = instance
        var visited = Set<Int32>()
        while current.pid > 1, visited.insert(current.pid).inserted {
            if let (prior, registration) = observed[current.pid], prior.sameBirth(as: current) {
                observed[pid] = (instance, registration)
                return registration
            }
            if let registration = roots[current.pid], current.matches(registration) {
                observed[pid] = (instance, registration)
                if observed.count > 4096 { prune() }
                return registration
            }
            guard let parent = ProcessInstance.read(current.parent) else { break }
            current = parent
        }
        observed.removeValue(forKey: pid)
        return nil
    }

    private func prune() {
        roots = roots.filter { ProcessInstance.read($0.key)?.matches($0.value) == true }
        observed = observed.filter { ProcessInstance.read($0.key)?.sameBirth(as: $0.value.0) == true }
    }
}
