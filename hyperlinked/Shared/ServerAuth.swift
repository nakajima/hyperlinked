import Foundation
import Security

enum ServerAuthMode: String, CaseIterable, Codable {
    case none
    case basic

    var label: String {
        switch self {
        case .none:
            return "None"
        case .basic:
            return "Basic Auth"
        }
    }
}

struct BasicAuthCredentials: Codable, Equatable {
    let username: String
    let password: String

    var normalized: BasicAuthCredentials {
        BasicAuthCredentials(
            username: username.trimmingCharacters(in: .whitespacesAndNewlines),
            password: password
        )
    }

    var isValid: Bool {
        let normalized = normalized
        return !normalized.username.isEmpty && !normalized.password.isEmpty
    }

    var authorizationHeaderValue: String? {
        let normalized = normalized
        guard normalized.isValid else {
            return nil
        }
        guard let encoded = "\(normalized.username):\(normalized.password)"
            .data(using: .utf8)?
            .base64EncodedString() else {
            return nil
        }
        return "Basic \(encoded)"
    }
}

final class ServerCredentialsStore {
    private let service = "fm.folder.hyperlinked.server-credentials.v1"
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    func saveCredentials(_ credentials: BasicAuthCredentials, for serverKey: String) -> Bool {
        let normalized = credentials.normalized
        guard normalized.isValid else {
            return false
        }
        guard let data = try? encoder.encode(normalized) else {
            return false
        }

        let query = baseQuery(serverKey: serverKey)
        SecItemDelete(query as CFDictionary)

        var insert = query
        insert[kSecValueData as String] = data
        insert[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock

        return SecItemAdd(insert as CFDictionary, nil) == errSecSuccess
    }

    func loadCredentials(for serverKey: String) -> BasicAuthCredentials? {
        var query = baseQuery(serverKey: serverKey)
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        query[kSecReturnData as String] = true

        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        guard status == errSecSuccess, let data = result as? Data else {
            return nil
        }
        guard let decoded = try? decoder.decode(BasicAuthCredentials.self, from: data) else {
            _ = deleteCredentials(for: serverKey)
            return nil
        }

        let normalized = decoded.normalized
        guard normalized.isValid else {
            _ = deleteCredentials(for: serverKey)
            return nil
        }
        return normalized
    }

    @discardableResult
    func deleteCredentials(for serverKey: String) -> Bool {
        let status = SecItemDelete(baseQuery(serverKey: serverKey) as CFDictionary)
        return status == errSecSuccess || status == errSecItemNotFound
    }

    private func baseQuery(serverKey: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: serverKey,
        ]
    }
}
