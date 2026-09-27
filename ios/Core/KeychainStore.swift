import Foundation
import Security

/// Credentials are stored only in the local Keychain, never preferences or log files.
public struct KeychainStore: Sendable {
    public let service: String
    public init(service: String = "com.riwork.remote.desktops") { self.service = service }
    public func read<T: Decodable>(_ type: T.Type, account: String = "instances") throws -> T? {
        var query = base(account)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data else { throw KeychainError(status: status) }
        return try JSONDecoder().decode(type, from: data)
    }
    public func write<T: Encodable>(_ value: T, account: String = "instances") throws {
        let data = try JSONEncoder().encode(value)
        let query = base(account)
        let status = SecItemUpdate(query as CFDictionary, [kSecValueData as String: data] as CFDictionary)
        if status == errSecItemNotFound {
            var item = query
            item[kSecValueData as String] = data
            item[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
            let add = SecItemAdd(item as CFDictionary, nil)
            guard add == errSecSuccess else { throw KeychainError(status: add) }
        } else if status != errSecSuccess { throw KeychainError(status: status) }
    }
    public func delete(account: String = "instances") throws {
        let status = SecItemDelete(base(account) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else { throw KeychainError(status: status) }
    }
    private func base(_ account: String) -> [String: Any] {
        [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service,
         kSecAttrAccount as String: account, kSecAttrSynchronizable as String: false]
    }
}

public struct KeychainError: Error, LocalizedError, Sendable {
    public let status: OSStatus
    public var errorDescription: String? { "Secure storage unavailable (\(status)). Unlock your device and try again." }
}
