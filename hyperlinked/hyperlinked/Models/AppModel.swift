import Foundation
import Combine

@MainActor
final class AppModel: ObservableObject {
    static let appGroupID = "group.fm.folder.hyperlinked"
    static let selectedServerURLKey = "selected_server_base_url"
    static let selectedServerAuthModeKey = "selected_server_auth_mode"

    private let logger = AppEventLogger(component: "AppModel")

    @Published var selectedServerURL: URL?
    @Published var selectedServerAuthMode: ServerAuthMode
    @Published var shouldShowServerSetup: Bool
    @Published private(set) var widgetRotationDiagnostics = WidgetRotationDiagnosticsSnapshot.empty

    private let defaults: UserDefaults
    private let sharedDefaults: UserDefaults?
    private let credentialsStore: ServerCredentialsStore
    private var offlineBackfillTask: Task<Void, Never>?
    private var offlineBackfillServerKey: String?

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

        logger.log(
            "app_model_initialized",
            details: [
                "selected_server": selectedServerURL?.absoluteString ?? "none",
                "auth_mode": selectedServerAuthMode.rawValue,
                "showing_server_setup": shouldShowServerSetup ? "true" : "false",
                "shared_defaults_available": sharedDefaults == nil ? "false" : "true",
            ]
        )

        refreshDiagnostics()
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
            logger.log(
                "save_server_connection_rejected",
                details: [
                    "reason": "invalid_url",
                    "input_url": url.absoluteString,
                    "auth_mode": authMode.rawValue,
                ]
            )
            return false
        }
        let normalized = normalizedURL.absoluteString
        let previous = selectedServerURL?.absoluteString
        let previousKey = selectedServerURL.map { Self.serverCredentialKey(for: $0) }
        let nextKey = Self.serverCredentialKey(for: normalizedURL)

        if previous != normalized {
            logger.log(
                "server_selection_changed",
                details: [
                    "previous_server": previous ?? "none",
                    "next_server": normalized,
                ]
            )
            offlineBackfillTask?.cancel()
            offlineBackfillTask = nil
            offlineBackfillServerKey = nil
            try? HyperlinkStore.openShared().clearAll()
            try? HyperlinkOfflineStore.openShared().clearAll()
        }
        if previousKey != nextKey, let previousKey {
            _ = credentialsStore.deleteCredentials(for: previousKey)
        }
        guard persistAuthSettings(
            mode: authMode,
            basicCredentials: basicCredentials,
            serverKey: nextKey
        ) else {
            logger.log(
                "save_server_connection_failed",
                details: [
                    "reason": "persist_auth_settings_failed",
                    "server": normalized,
                    "auth_mode": authMode.rawValue,
                ]
            )
            return false
        }

        selectedServerURL = normalizedURL
        selectedServerAuthMode = authMode
        sharedDefaults?.set(normalizedURL.absoluteString, forKey: Self.selectedServerURLKey)
        defaults.set(normalizedURL.absoluteString, forKey: Self.selectedServerURLKey)
        sharedDefaults?.set(authMode.rawValue, forKey: Self.selectedServerAuthModeKey)
        defaults.set(authMode.rawValue, forKey: Self.selectedServerAuthModeKey)
        shouldShowServerSetup = false
        logger.log(
            "server_connection_saved",
            details: [
                "server": normalizedURL.absoluteString,
                "auth_mode": authMode.rawValue,
                "credentials_present": basicCredentials == nil ? "false" : "true",
            ]
        )
        return true
    }

    @discardableResult
    func updateSelectedServerAuth(
        mode: ServerAuthMode,
        basicCredentials: BasicAuthCredentials?
    ) -> Bool {
        guard let selectedServerURL else {
            logger.log(
                "update_server_auth_rejected",
                details: ["reason": "no_selected_server", "auth_mode": mode.rawValue]
            )
            return false
        }
        let serverKey = Self.serverCredentialKey(for: selectedServerURL)
        guard persistAuthSettings(
            mode: mode,
            basicCredentials: basicCredentials,
            serverKey: serverKey
        ) else {
            logger.log(
                "update_server_auth_failed",
                details: [
                    "server": selectedServerURL.absoluteString,
                    "auth_mode": mode.rawValue,
                ]
            )
            return false
        }

        selectedServerAuthMode = mode
        sharedDefaults?.set(mode.rawValue, forKey: Self.selectedServerAuthModeKey)
        defaults.set(mode.rawValue, forKey: Self.selectedServerAuthModeKey)
        logger.log(
            "server_auth_updated",
            details: [
                "server": selectedServerURL.absoluteString,
                "auth_mode": mode.rawValue,
                "credentials_present": basicCredentials == nil ? "false" : "true",
            ]
        )
        return true
    }

    func openServerSetup() {
        shouldShowServerSetup = true
        logger.log("server_setup_opened")
    }

    func dismissServerSetup() {
        shouldShowServerSetup = false
        logger.log("server_setup_dismissed")
    }

    func refreshDiagnostics() {
        logger.log("diagnostics_refresh_requested")
        Task {
            let snapshot = await AppDiagnosticsLog.shared.refreshSnapshot()
            widgetRotationDiagnostics = snapshot
            logger.log(
                "diagnostics_refresh_completed",
                details: [
                    "status": snapshot.status.label,
                    "log_size_bytes": String(snapshot.logFileSizeBytes),
                    "recent_failures_24h": String(snapshot.recentFailureCount24h),
                ]
            )
        }
    }

    func startOfflineBackfillIfNeeded(force: Bool = false) {
        guard let client = apiClient,
              let selectedServerURL else {
            logger.log(
                "offline_backfill_skipped",
                details: ["reason": "missing_client_or_server", "force": force ? "true" : "false"]
            )
            return
        }

        let serverKey = selectedServerURL.absoluteString
        if !force, offlineBackfillTask != nil, offlineBackfillServerKey == serverKey {
            logger.log(
                "offline_backfill_skipped",
                details: ["reason": "already_running", "server": serverKey]
            )
            return
        }

        offlineBackfillTask?.cancel()
        offlineBackfillServerKey = serverKey
        logger.log(
            "offline_backfill_started",
            details: ["server": serverKey, "force": force ? "true" : "false"]
        )
        offlineBackfillTask = Task {
            defer {
                Task { @MainActor in
                    if self.offlineBackfillServerKey == serverKey {
                        self.offlineBackfillTask = nil
                    }
                }
            }

            guard let store = try? HyperlinkStore.openShared(),
                  let hyperlinks = try? store.fetchAll(),
                  !hyperlinks.isEmpty else {
                self.logger.log(
                    "offline_backfill_skipped",
                    details: ["reason": "no_cached_hyperlinks", "server": serverKey]
                )
                return
            }

            self.logger.log(
                "offline_backfill_processing",
                details: ["server": serverKey, "hyperlink_count": String(hyperlinks.count)]
            )
            await HyperlinkOfflineSnapshotManager.shared.backfillMissingSnapshots(
                hyperlinks: hyperlinks,
                client: client
            )
            self.logger.log(
                "offline_backfill_completed",
                details: ["server": serverKey, "hyperlink_count": String(hyperlinks.count)]
            )
        }
    }

    func clearDiagnosticsLog() {
        logger.log("diagnostics_clear_requested")
        Task {
            let snapshot = await AppDiagnosticsLog.shared.clearLog()
            widgetRotationDiagnostics = snapshot
            logger.log("diagnostics_cleared")
        }
    }

    func resetServerSelection() {
        logger.log(
            "server_selection_reset_requested",
            details: ["selected_server": selectedServerURL?.absoluteString ?? "none"]
        )
        offlineBackfillTask?.cancel()
        offlineBackfillTask = nil
        offlineBackfillServerKey = nil
        try? HyperlinkStore.openShared().clearAll()
        try? HyperlinkOfflineStore.openShared().clearAll()
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
        logger.log("server_selection_reset_completed")
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
                logger.log(
                    "persist_auth_settings_rejected",
                    details: ["reason": "missing_basic_credentials", "server_key": serverKey]
                )
                return false
            }
            let saved = credentialsStore.saveCredentials(credentials, for: serverKey)
            if !saved {
                logger.log(
                    "persist_auth_settings_failed",
                    details: ["reason": "credentials_store_save_failed", "server_key": serverKey]
                )
            }
            return saved
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
