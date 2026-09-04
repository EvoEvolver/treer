import Foundation
import Security

protocol TokenStore {
    func readToken(account: String) throws -> String?
    func writeToken(_ token: String, account: String) throws
    func deleteToken(account: String) throws
}

enum TokenStoreError: Error {
    case unexpectedStatus(OSStatus)
    case invalidData
}

final class FallbackTokenStore: TokenStore {
    private let primary: TokenStore
    private let defaults: UserDefaults
    private let prefix = "treer.token."

    init(primary: TokenStore = KeychainTokenStore(), defaults: UserDefaults = .standard) {
        self.primary = primary
        self.defaults = defaults
    }

    func readToken(account: String) throws -> String? {
        do {
            if let token = try primary.readToken(account: account) {
                return token
            }
        } catch TokenStoreError.unexpectedStatus(let status) where status == errSecMissingEntitlement {
            // Simulator/unsigned builds cannot use Keychain; UserDefaults holds the session.
        }
        return defaults.string(forKey: prefix + account)
    }

    func writeToken(_ token: String, account: String) throws {
        do {
            try primary.writeToken(token, account: account)
            defaults.removeObject(forKey: prefix + account)
        } catch TokenStoreError.unexpectedStatus(let status) where status == errSecMissingEntitlement {
            defaults.set(token, forKey: prefix + account)
        }
    }

    func deleteToken(account: String) throws {
        do {
            try primary.deleteToken(account: account)
        } catch TokenStoreError.unexpectedStatus(let status) where status == errSecMissingEntitlement {
            // Ignore unsigned Keychain failures.
        }
        defaults.removeObject(forKey: prefix + account)
    }
}

final class KeychainTokenStore: TokenStore {
    private let service: String

    init(service: String = "ai.treer.mobile") {
        self.service = service
    }

    func readToken(account: String) throws -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        if status == errSecItemNotFound {
            return nil
        }
        guard status == errSecSuccess else {
            throw TokenStoreError.unexpectedStatus(status)
        }
        guard let data = item as? Data, let token = String(data: data, encoding: .utf8) else {
            throw TokenStoreError.invalidData
        }
        return token
    }

    func writeToken(_ token: String, account: String) throws {
        let data = Data(token.utf8)
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account
        ]
        let attributes: [String: Any] = [
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        ]
        let updateStatus = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
        if updateStatus == errSecSuccess {
            return
        }
        guard updateStatus == errSecItemNotFound else {
            throw TokenStoreError.unexpectedStatus(updateStatus)
        }
        var addQuery = query
        attributes.forEach { addQuery[$0.key] = $0.value }
        let addStatus = SecItemAdd(addQuery as CFDictionary, nil)
        guard addStatus == errSecSuccess else {
            throw TokenStoreError.unexpectedStatus(addStatus)
        }
    }

    func deleteToken(account: String) throws {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account
        ]
        let status = SecItemDelete(query as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw TokenStoreError.unexpectedStatus(status)
        }
    }
}

final class MemoryTokenStore: TokenStore {
    private var values: [String: String] = [:]

    func readToken(account: String) throws -> String? {
        values[account]
    }

    func writeToken(_ token: String, account: String) throws {
        values[account] = token
    }

    func deleteToken(account: String) throws {
        values.removeValue(forKey: account)
    }
}
