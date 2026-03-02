import Foundation
import Security

enum ShareServerAuthMode: String {
    case none
    case basic
}

struct ShareBasicAuthCredentials: Codable {
    let username: String
    let password: String

    var normalized: ShareBasicAuthCredentials {
        ShareBasicAuthCredentials(
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

final class ShareServerCredentialsStore {
    private let service = "fm.folder.hyperlinked.server-credentials.v1"
    private let decoder = JSONDecoder()

    func loadCredentials(for serverKey: String) -> ShareBasicAuthCredentials? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: serverKey,
            kSecMatchLimit as String: kSecMatchLimitOne,
            kSecReturnData as String: true,
        ]

        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        guard status == errSecSuccess, let data = result as? Data else {
            return nil
        }
        guard let decoded = try? decoder.decode(ShareBasicAuthCredentials.self, from: data) else {
            return nil
        }
        let normalized = decoded.normalized
        guard normalized.isValid else {
            return nil
        }
        return normalized
    }
}
