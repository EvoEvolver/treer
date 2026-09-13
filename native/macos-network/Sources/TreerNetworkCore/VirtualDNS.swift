import Foundation
import Network

/// Provider-wide names and addresses: one system DNS cache cannot use a
/// different synthetic address assignment for each Controller/workspace.
public struct VirtualDNS: Codable {
    private var addresses: [String: UInt32] = [:]
    private var controllers: [UInt16: [String]] = [:]
    private var next: UInt32 = 1

    public init() {}

    private enum CodingKeys: String, CodingKey { case addresses, controllers, next }
    public init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        addresses = try values.decode([String: UInt32].self, forKey: .addresses)
        controllers = try values.decode([UInt16: [String]].self, forKey: .controllers)
        next = try values.decode(UInt32.self, forKey: .next)
        guard addresses.count <= 131070, Set(addresses.values).count == addresses.count,
              next > (addresses.values.max() ?? 0), next <= 131071,
              addresses.values.allSatisfy({ $0 > 0 && $0 < 131071 }),
              addresses.keys.allSatisfy({ (try? Self.normalize($0)) == $0 }),
              controllers.allSatisfy({ port, names in
                  port > 0 && names.count <= 16384 && names.allSatisfy { addresses[$0] != nil }
              }) else { throw NativeNetworkError.invalidArguments }
    }

    public static func normalize(_ host: String) throws -> String {
        let name = host.lowercased().trimmingCharacters(in: CharacterSet(charactersIn: "."))
        let labels = name.split(separator: ".", omittingEmptySubsequences: false)
        guard !name.isEmpty, name.utf8.count <= 253, labels.count > 1,
              labels.allSatisfy({ label in
                  !label.isEmpty && label.utf8.count <= 63 && label.first != "-" && label.last != "-"
                  && label.utf8.allSatisfy { (97...122).contains($0) || (48...57).contains($0) || $0 == 45 }
              }) else { throw NativeNetworkError.invalidArguments }
        return name
    }

    public mutating func replace(controller: UInt16, hosts: [String]) throws {
        guard controller > 0, hosts.count <= 16384 else { throw NativeNetworkError.invalidArguments }
        let names = try Set(hosts.map(Self.normalize)).sorted()
        guard addresses.count + names.filter({ addresses[$0] == nil }).count <= 131070 else {
            throw NativeNetworkError.unavailable
        }
        for name in names where addresses[name] == nil {
            addresses[name] = next
            next += 1
        }
        controllers[controller] = names
    }

    public var activeNames: [String] { Array(Set(controllers.values.flatMap { $0 })).sorted() }

    public static func canonicalAddress(_ host: String) -> String {
        guard let ipv6 = IPv6Address(host) else { return host }
        let bytes = Array(ipv6.rawValue)
        if bytes.prefix(10).allSatisfy({ $0 == 0 }), bytes[10] == 255, bytes[11] == 255 {
            return bytes.suffix(4).map(String.init).joined(separator: ".")
        }
        return host
    }

    /// Retain retired mappings so cached DNS answers can never reach a different
    /// name. The Controller/Proxy still authorizes the original destination.
    public var reverse: [String: String] {
        Dictionary(uniqueKeysWithValues: addresses.map { name, number in
            let bytes = Self.bytes(number)
            return (bytes.map(String.init).joined(separator: "."), name)
        })
    }

    private static func bytes(_ number: UInt32) -> [UInt8] {
        let value = 0xc6120000 + number // RFC 2544 benchmarking range, 198.18/15.
        return [UInt8(value >> 24), UInt8((value >> 16) & 255), UInt8((value >> 8) & 255), UInt8(value & 255)]
    }

    /// Answer one ordinary DNS question. No recursion, external forwarding or
    /// wildcard answers. A/AAAA share one mapping; AAAA gets NOERROR/NODATA so
    /// dual-stack callers can use the synthetic A record.
    public func answer(_ packet: Data) throws -> Data {
        let input = [UInt8](packet)
        guard input.count >= 12, input[2] & 0xf8 == 0,
              input[4] == 0, input[5] == 1 else { throw NativeNetworkError.invalidArguments }
        var cursor = 12
        var labels: [String] = []
        var visited = Set<Int>()
        var end: Int?
        while true {
            guard cursor < input.count, visited.insert(cursor).inserted, visited.count < 128 else {
                throw NativeNetworkError.invalidArguments
            }
            let count = Int(input[cursor])
            if count & 0xc0 == 0xc0 {
                guard cursor + 1 < input.count else { throw NativeNetworkError.invalidArguments }
                if end == nil { end = cursor + 2 }
                cursor = (count & 63) * 256 + Int(input[cursor + 1])
                continue
            }
            guard count <= 63, cursor + 1 + count <= input.count else { throw NativeNetworkError.invalidArguments }
            cursor += 1
            if count == 0 { break }
            guard let label = String(bytes: input[cursor..<(cursor + count)], encoding: .ascii) else {
                throw NativeNetworkError.invalidArguments
            }
            labels.append(label)
            cursor += count
        }
        let questionEnd = end ?? cursor
        guard questionEnd + 4 <= input.count else { throw NativeNetworkError.invalidArguments }
        let name = try Self.normalize(labels.joined(separator: "."))
        let type = UInt16(input[questionEnd]) * 256 + UInt16(input[questionEnd + 1])
        let internet = input[questionEnd + 2] == 0 && input[questionEnd + 3] == 1
        let exists = controllers.values.contains { $0.contains(name) }
        let address = exists && internet && type == 1 ? addresses[name] : nil
        var output = [input[0], input[1], UInt8(0x84 | (input[2] & 1)), UInt8(exists ? 0 : 3),
                      0, 1, 0, UInt8(address == nil ? 0 : 1), 0, 0, 0, 0]
        // Re-encode the question, rather than returning compression pointers
        // that may refer to discarded additional records.
        for label in labels {
            output.append(UInt8(label.utf8.count)); output += label.utf8
        }
        output += [0] + Array(input[questionEnd..<(questionEnd + 4)])
        if let address {
            output += [0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 1, 0, 4] + Self.bytes(address)
        }
        return Data(output)
    }
}
