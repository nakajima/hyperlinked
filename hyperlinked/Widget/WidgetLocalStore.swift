import Foundation
import OSLog
import SQLite3

struct WidgetLocalStore {
    private static let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "fm.folder.hyperlinked.Widget",
        category: "local-cache"
    )
    private static let databaseFilename = "db.sqlite"
    private static let tableName = "hyperlink_records"
    private static let appGroupID = "group.fm.folder.hyperlinked"

    func listHyperlinks(configuration: ConfigurationAppIntent, limit: Int) throws -> [WidgetHyperlink] {
        guard limit > 0 else {
            return []
        }

        guard let databaseURL = Self.databaseURL(),
              FileManager.default.fileExists(atPath: databaseURL.path) else {
            return []
        }

        var database: OpaquePointer?
        let openCode = sqlite3_open_v2(
            databaseURL.path,
            &database,
            SQLITE_OPEN_READONLY | SQLITE_OPEN_NOMUTEX,
            nil
        )
        guard openCode == SQLITE_OK, let database else {
            let message = database.flatMap { sqlite3_errmsg($0) }.map { String(cString: $0) } ?? "unknown"
            if let database {
                sqlite3_close(database)
            }
            throw NSError(
                domain: "WidgetLocalStore",
                code: Int(openCode),
                userInfo: [NSLocalizedDescriptionKey: "failed to open cache database: \(message)"]
            )
        }
        defer { sqlite3_close(database) }

        let sql = buildSelectSQL(configuration: configuration)
        var statement: OpaquePointer?
        let prepareCode = sqlite3_prepare_v2(database, sql, -1, &statement, nil)
        guard prepareCode == SQLITE_OK, let statement else {
            let message = String(cString: sqlite3_errmsg(database))
            throw NSError(
                domain: "WidgetLocalStore",
                code: Int(prepareCode),
                userInfo: [NSLocalizedDescriptionKey: "failed to prepare cache query: \(message)"]
            )
        }
        defer { sqlite3_finalize(statement) }

        sqlite3_bind_int(statement, 1, Int32(limit))

        var hyperlinks: [WidgetHyperlink] = []
        while sqlite3_step(statement) == SQLITE_ROW {
            guard let row = hyperlink(from: statement) else {
                continue
            }
            hyperlinks.append(row)
        }
        return hyperlinks
    }

    private func hyperlink(from statement: OpaquePointer?) -> WidgetHyperlink? {
        let id = Int(sqlite3_column_int(statement, 0))
        guard let titleRaw = Self.stringColumn(statement, index: 1),
              let urlRaw = Self.stringColumn(statement, index: 2),
              let visitURL = URL(string: urlRaw) else {
            return nil
        }

        let hostRaw = Self.stringColumn(statement, index: 3)
        let host = normalizedHost(rawHost: hostRaw, fallbackURL: visitURL)
        let title = normalizeDisplayText(titleRaw).ifEmpty(host)

        let oneLinerRaw = Self.stringColumn(statement, index: 4)
        let oneLiner = normalizeDisplayText(oneLinerRaw ?? "").ifEmpty(host)

        let thumbnailURL = Self.stringColumn(statement, index: 5).flatMap(URL.init(string:))
        let thumbnailDarkURL = Self.stringColumn(statement, index: 6).flatMap(URL.init(string:))

        return WidgetHyperlink(
            id: id,
            title: title,
            url: urlRaw,
            host: host,
            oneLiner: oneLiner,
            visitURL: visitURL,
            faviconURL: nil,
            thumbnailURL: thumbnailURL,
            thumbnailDarkURL: thumbnailDarkURL,
            fallbackColor: nil
        )
    }

    private func normalizedHost(rawHost: String?, fallbackURL: URL) -> String {
        if let rawHost {
            let normalizedRaw = normalizeDisplayText(rawHost).lowercased()
            if !normalizedRaw.isEmpty {
                if normalizedRaw.hasPrefix("www.") {
                    return String(normalizedRaw.dropFirst(4))
                }
                return normalizedRaw
            }
        }

        let fallback = fallbackURL.host?.lowercased() ?? fallbackURL.absoluteString
        if fallback.hasPrefix("www.") {
            return String(fallback.dropFirst(4))
        }
        return fallback
    }

    private static func databaseURL() -> URL? {
        guard let containerURL = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: appGroupID
        ) else {
            logger.debug("Widget local cache app group container unavailable")
            return nil
        }

        let appSupportURL = containerURL
            .appendingPathComponent("Library", isDirectory: true)
            .appendingPathComponent("Application Support", isDirectory: true)
        return appSupportURL.appendingPathComponent(databaseFilename, isDirectory: false)
    }

    private func buildSelectSQL(configuration: ConfigurationAppIntent) -> String {
        var filters: [String] = []

        switch configuration.scope {
        case .rootOnly:
            filters.append("discovery_depth = 0")
        case .discoveredOnly:
            filters.append("discovery_depth > 0")
        case .all:
            break
        }

        if configuration.unclickedOnly {
            filters.append("clicks_count = 0")
        }

        let whereClause = filters.isEmpty ? "" : "WHERE \(filters.joined(separator: " AND "))"
        let orderClause: String = {
            switch configuration.sortOrder {
            case .newest:
                return "created_at DESC, id DESC"
            case .oldest:
                return "created_at ASC, id ASC"
            case .random:
                return "RANDOM()"
            }
        }()

        return """
            SELECT
                id,
                title,
                url,
                host,
                one_liner,
                thumbnail_url,
                thumbnail_dark_url
            FROM \(Self.tableName)
            \(whereClause)
            ORDER BY \(orderClause)
            LIMIT ?
        """
    }

    private static func stringColumn(_ statement: OpaquePointer?, index: Int32) -> String? {
        guard let raw = sqlite3_column_text(statement, index) else {
            return nil
        }
        return String(cString: raw)
    }

    private func normalizeDisplayText(_ value: String) -> String {
        let collapsed = value.replacingOccurrences(
            of: #"\s+"#,
            with: " ",
            options: .regularExpression
        )
        return collapsed.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

private extension String {
    func ifEmpty(_ fallback: String) -> String {
        isEmpty ? fallback : self
    }
}
