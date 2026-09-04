import Foundation

enum TerminalBinaryKind: UInt8 {
    case ready = 1
    case output = 2
    case input = 3
}

struct TerminalBinaryFrame: Equatable {
    static let version: UInt8 = 1
    static let headerLength = 12

    var kind: TerminalBinaryKind
    var sessionId: String
    var revision: UInt64
    var payload: Data

    func encode() throws -> Data {
        let session = Data(sessionId.utf8)
        guard !session.isEmpty else {
            throw TerminalFrameError.emptySession
        }
        guard session.count <= Int(UInt16.max) else {
            throw TerminalFrameError.sessionTooLong
        }
        var data = Data(capacity: Self.headerLength + session.count + payload.count)
        data.append(Self.version)
        data.append(kind.rawValue)
        var sessionLen = UInt16(session.count).bigEndian
        withUnsafeBytes(of: &sessionLen) { data.append(contentsOf: $0) }
        var revisionBE = revision.bigEndian
        withUnsafeBytes(of: &revisionBE) { data.append(contentsOf: $0) }
        data.append(session)
        data.append(payload)
        return data
    }

    static func decode(_ encoded: Data) throws -> TerminalBinaryFrame {
        guard encoded.count >= headerLength else {
            throw TerminalFrameError.tooShort
        }
        let bytes = [UInt8](encoded)
        guard bytes[0] == version else {
            throw TerminalFrameError.versionMismatch(bytes[0])
        }
        guard let kind = TerminalBinaryKind(rawValue: bytes[1]) else {
            throw TerminalFrameError.unknownKind(bytes[1])
        }
        let sessionLen = Int(UInt16(bytes[2]) << 8 | UInt16(bytes[3]))
        let payloadOffset = headerLength + sessionLen
        guard sessionLen > 0, payloadOffset <= encoded.count else {
            throw TerminalFrameError.invalidSession
        }
        var revision: UInt64 = 0
        for i in 0 ..< 8 {
            revision = (revision << 8) | UInt64(bytes[4 + i])
        }
        guard let sessionId = String(data: encoded.subdata(in: headerLength ..< payloadOffset), encoding: .utf8) else {
            throw TerminalFrameError.invalidSession
        }
        return TerminalBinaryFrame(
            kind: kind,
            sessionId: sessionId,
            revision: revision,
            payload: encoded.subdata(in: payloadOffset ..< encoded.count)
        )
    }
}

enum TerminalFrameError: Error, Equatable {
    case emptySession
    case sessionTooLong
    case tooShort
    case versionMismatch(UInt8)
    case unknownKind(UInt8)
    case invalidSession
}

enum ANSIStripper {
    static func strip(_ text: String) -> String {
        var result = text
        result = result.replacingOccurrences(of: "\u{1b}\\][^\\u{07}]*\\u{07}", with: "", options: .regularExpression)
        result = result.replacingOccurrences(of: "\u{1b}\\[[0-9;?=]*[ -/]*[@-~]", with: "", options: .regularExpression)
        result = result.replacingOccurrences(of: "\u{1b}[()][0-9AB]", with: "", options: .regularExpression)
        result = result.replacingOccurrences(of: "\u{1b}[NO]", with: "", options: .regularExpression)
        result = result.replacingOccurrences(of: "\r\n", with: "\n")
        result = result.replacingOccurrences(of: "\r", with: "\n")
        return result
    }
}
