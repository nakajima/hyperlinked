import Foundation
import Combine

@MainActor
final class AppModel: ObservableObject {
    static let appGroupID = "group.fm.folder.hyperlinked"
    static let selectedServerURLKey = "selected_server_base_url"

    @Published var selectedServerURL: URL?
    @Published var shouldShowServerSetup: Bool

    private let defaults: UserDefaults
    private let sharedDefaults: UserDefaults?

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        self.sharedDefaults = UserDefaults(suiteName: Self.appGroupID)

        let resolvedDefaults = sharedDefaults ?? defaults

        if let raw = resolvedDefaults.string(forKey: Self.selectedServerURLKey),
           let parsed = AppModel.normalizedServerURL(from: raw) {
            selectedServerURL = parsed
            shouldShowServerSetup = false
        } else {
            selectedServerURL = nil
            shouldShowServerSetup = true
        }
    }

    var apiClient: APIClient? {
        guard let selectedServerURL else {
            return nil
        }
        return APIClient(baseURL: selectedServerURL)
    }

    func saveServerURL(_ url: URL) {
        guard let normalizedURL = Self.normalizedServerURL(from: url.absoluteString) else {
            return
        }
        let normalized = normalizedURL.absoluteString
        let previous = selectedServerURL?.absoluteString

        if previous != normalized {
            try? HyperlinkStore.openShared().clearAll()
        }

        selectedServerURL = normalizedURL
        sharedDefaults?.set(normalizedURL.absoluteString, forKey: Self.selectedServerURLKey)
        defaults.set(normalizedURL.absoluteString, forKey: Self.selectedServerURLKey)
        shouldShowServerSetup = false
    }

    func openServerSetup() {
        shouldShowServerSetup = true
    }

    func dismissServerSetup() {
        shouldShowServerSetup = false
    }

    func resetServerSelection() {
        try? HyperlinkStore.openShared().clearAll()
        sharedDefaults?.removeObject(forKey: Self.selectedServerURLKey)
        defaults.removeObject(forKey: Self.selectedServerURLKey)
        selectedServerURL = nil
        shouldShowServerSetup = true
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
