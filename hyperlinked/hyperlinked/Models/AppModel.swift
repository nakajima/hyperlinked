import Foundation
import Combine

@MainActor
final class AppModel: ObservableObject {
    @Published var selectedServerURL: URL?
    @Published var shouldShowServerSetup: Bool

    private let defaults: UserDefaults
    private let selectedServerURLKey = "selected_server_base_url"

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        if let raw = defaults.string(forKey: selectedServerURLKey),
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
        let normalized = url.absoluteString.trimmingCharacters(in: .whitespacesAndNewlines)
        selectedServerURL = URL(string: normalized)
        defaults.set(normalized, forKey: selectedServerURLKey)
        shouldShowServerSetup = false
    }

    func openServerSetup() {
        shouldShowServerSetup = true
    }

    func dismissServerSetup() {
        shouldShowServerSetup = false
    }

    func resetServerSelection() {
        defaults.removeObject(forKey: selectedServerURLKey)
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
