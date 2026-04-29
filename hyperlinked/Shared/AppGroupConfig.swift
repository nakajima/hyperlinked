import Foundation

enum AppGroupConfig {
    static let appGroupID = "group.fm.folder.hyperlinked"
    static let databaseFilename = "db.sqlite"

    enum DefaultsKey {
        static let selectedServerURL = "selected_server_base_url"
        static let selectedServerAuthMode = "selected_server_auth_mode"
    }

    enum DiagnosticsKey {
        static let pendingEvents = "diagnostics.pending_events.v1"
        static let ingestedEventIDs = "diagnostics.ingested_event_ids.v1"
        static let lastFailureAt = "diagnostics.widget_rotation.last_failure_at.v1"
        static let lastFailureDBMode = "diagnostics.widget_rotation.last_failure_db_mode.v1"
        static let lastFailureSQLiteCode = "diagnostics.widget_rotation.last_failure_sqlite_code.v1"
        static let lastFailureSQLiteMessage = "diagnostics.widget_rotation.last_failure_sqlite_message.v1"
        static let lastFailureStage = "diagnostics.widget_rotation.last_failure_stage.v1"
        static let lastSuccessAt = "diagnostics.widget_rotation.last_success_at.v1"
    }
}

enum ServerConnectionSettings {
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
