import Foundation
import Combine

@MainActor
final class AppModel: ObservableObject {
    static let appGroupID = "group.fm.folder.hyperlinked"
    static let selectedServerURLKey = "selected_server_base_url"
    static let selectedServerAuthModeKey = "selected_server_auth_mode"

    @Published var selectedServerURL: URL?
    @Published var selectedServerAuthMode: ServerAuthMode
    @Published var shouldShowServerSetup: Bool

    private let defaults: UserDefaults
    private let sharedDefaults: UserDefaults?
    private let credentialsStore: ServerCredentialsStore

    init(
        defaults: UserDefaults = .standard,
        credentialsStore: ServerCredentialsStore? = nil
    ) {
        self.defaults = defaults
        self.sharedDefaults = UserDefaults(suiteName: Self.appGroupID)
        self.credentialsStore = credentialsStore ?? ServerCredentialsStore()

        let resolvedDefaults = sharedDefaults ?? defaults
        let savedMode = ServerAuthMode(
            rawValue: resolvedDefaults.string(forKey: Self.selectedServerAuthModeKey) ?? ""
        ) ?? .none

        if let raw = resolvedDefaults.string(forKey: Self.selectedServerURLKey),
           let parsed = AppModel.normalizedServerURL(from: raw) {
            selectedServerURL = parsed
            selectedServerAuthMode = savedMode
            shouldShowServerSetup = false
        } else {
            selectedServerURL = nil
            selectedServerAuthMode = .none
            shouldShowServerSetup = true
        }
    }

    var apiClient: APIClient? {
        guard let selectedServerURL else {
            return nil
        }
        let header = selectedBasicCredentials()?.authorizationHeaderValue
        return APIClient(baseURL: selectedServerURL, authorizationHeaderValue: header)
    }

    func selectedBasicCredentials() -> BasicAuthCredentials? {
        guard selectedServerAuthMode == .basic,
              let selectedServerURL else {
            return nil
        }
        return credentialsStore.loadCredentials(for: Self.serverCredentialKey(for: selectedServerURL))
    }

    func saveServerURL(_ url: URL) {
        _ = saveServerConnection(url, authMode: .none, basicCredentials: nil)
    }

    @discardableResult
    func saveServerConnection(
        _ url: URL,
        authMode: ServerAuthMode,
        basicCredentials: BasicAuthCredentials?
    ) -> Bool {
        guard let normalizedURL = Self.normalizedServerURL(from: url.absoluteString) else {
            return false
        }
        let normalized = normalizedURL.absoluteString
        let previous = selectedServerURL?.absoluteString
        let previousKey = selectedServerURL.map { Self.serverCredentialKey(for: $0) }
        let nextKey = Self.serverCredentialKey(for: normalizedURL)

        if previous != normalized {
            try? HyperlinkStore.openShared().clearAll()
        }
        if previousKey != nextKey, let previousKey {
            _ = credentialsStore.deleteCredentials(for: previousKey)
        }
        guard persistAuthSettings(
            mode: authMode,
            basicCredentials: basicCredentials,
            serverKey: nextKey
        ) else {
            return false
        }

        selectedServerURL = normalizedURL
        selectedServerAuthMode = authMode
        sharedDefaults?.set(normalizedURL.absoluteString, forKey: Self.selectedServerURLKey)
        defaults.set(normalizedURL.absoluteString, forKey: Self.selectedServerURLKey)
        sharedDefaults?.set(authMode.rawValue, forKey: Self.selectedServerAuthModeKey)
        defaults.set(authMode.rawValue, forKey: Self.selectedServerAuthModeKey)
        shouldShowServerSetup = false
        return true
    }

    @discardableResult
    func updateSelectedServerAuth(
        mode: ServerAuthMode,
        basicCredentials: BasicAuthCredentials?
    ) -> Bool {
        guard let selectedServerURL else {
            return false
        }
        let serverKey = Self.serverCredentialKey(for: selectedServerURL)
        guard persistAuthSettings(
            mode: mode,
            basicCredentials: basicCredentials,
            serverKey: serverKey
        ) else {
            return false
        }

        selectedServerAuthMode = mode
        sharedDefaults?.set(mode.rawValue, forKey: Self.selectedServerAuthModeKey)
        defaults.set(mode.rawValue, forKey: Self.selectedServerAuthModeKey)
        return true
    }

    func openServerSetup() {
        shouldShowServerSetup = true
    }

    func dismissServerSetup() {
        shouldShowServerSetup = false
    }

    func resetServerSelection() {
        try? HyperlinkStore.openShared().clearAll()
        if let selectedServerURL {
            _ = credentialsStore.deleteCredentials(for: Self.serverCredentialKey(for: selectedServerURL))
        }
        sharedDefaults?.removeObject(forKey: Self.selectedServerURLKey)
        sharedDefaults?.removeObject(forKey: Self.selectedServerAuthModeKey)
        defaults.removeObject(forKey: Self.selectedServerURLKey)
        defaults.removeObject(forKey: Self.selectedServerAuthModeKey)
        selectedServerURL = nil
        selectedServerAuthMode = .none
        shouldShowServerSetup = true
    }

    private func persistAuthSettings(
        mode: ServerAuthMode,
        basicCredentials: BasicAuthCredentials?,
        serverKey: String
    ) -> Bool {
        switch mode {
        case .none:
            _ = credentialsStore.deleteCredentials(for: serverKey)
            return true
        case .basic:
            guard let credentials = basicCredentials else {
                return false
            }
            return credentialsStore.saveCredentials(credentials, for: serverKey)
        }
    }

    static func serverCredentialKey(for url: URL) -> String {
        guard let normalized = normalizedServerURL(from: url.absoluteString) else {
            return url.absoluteString.lowercased()
        }
        return normalized.absoluteString.lowercased()
    }

    static func normalizedServerURL(from rawValue: String) -> URL? {
        let trimmed = rawValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            return nil
        }

        let candidate = trimmed.contains("://") ? trimmed : "http://\(trimmed)"
        guard var components = URLComponents(string: candidate),
              let scheme = components.scheme?.lowercased(),
              (scheme == "http" || scheme == "https"),
              let host = components.host,
              !host.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return nil
        }

        components.user = nil
        components.password = nil
        components.path = ""
        components.query = nil
        components.fragment = nil

        guard let url = components.url else {
            return nil
        }

        let absolute = url.absoluteString
        if absolute.hasSuffix("/") {
            return URL(string: String(absolute.dropLast()))
        }
        return url
    }
}
