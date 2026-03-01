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
    private static let lastShownInWidgetColumn = "last_shown_in_widget"
    private static let appGroupID = "group.fm.folder.hyperlinked"
    private static let rotationWindow: TimeInterval = 20 * 60
    private static let sqliteTransient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)
    private static let shownAtFormatter: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        formatter.timeZone = TimeZone(secondsFromGMT: 0)
        return formatter
    }()

    private struct OpenedDatabase {
        let handle: OpaquePointer
        let canWrite: Bool
    }

    func listHyperlinks(configuration: ConfigurationAppIntent, limit: Int) throws -> [WidgetHyperlink] {
        guard limit > 0 else {
            return []
        }

        guard let databaseURL = Self.databaseURL(),
              FileManager.default.fileExists(atPath: databaseURL.path) else {
            return []
        }

        let opened = try openDatabase(path: databaseURL.path)
        let database = opened.handle
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 1_000)

        let supportsLastShownInWidget = Self.columnExists(
            Self.lastShownInWidgetColumn,
            in: Self.tableName,
            database: database
        )
        let now = Date()
        let cutoff = now.addingTimeInterval(-Self.rotationWindow)
        let shouldPrioritizeUnshown = configuration.rotateSlowly && supportsLastShownInWidget
        let sql = buildSelectSQL(
            configuration: configuration,
            prioritizeUnshown: shouldPrioritizeUnshown
        )
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

        var bindIndex: Int32 = 1
        if shouldPrioritizeUnshown {
            let cutoffText = Self.shownAtFormatter.string(from: cutoff)
            guard Self.bindText(cutoffText, to: statement, index: bindIndex) == SQLITE_OK else {
                throw NSError(
                    domain: "WidgetLocalStore",
                    code: Int(sqlite3_errcode(database)),
                    userInfo: [NSLocalizedDescriptionKey: "failed to bind rotation cutoff"]
                )
            }
            bindIndex += 1
        }

        sqlite3_bind_int(statement, bindIndex, Int32(limit))

        var hyperlinks: [WidgetHyperlink] = []
        while sqlite3_step(statement) == SQLITE_ROW {
            guard let row = hyperlink(from: statement) else {
                continue
            }
            hyperlinks.append(row)
        }

        if supportsLastShownInWidget, opened.canWrite, !hyperlinks.isEmpty {
            let shownAt = Self.shownAtFormatter.string(from: now)
            do {
                try updateLastShownInWidget(
                    hyperlinkIDs: hyperlinks.map(\.id),
                    shownAt: shownAt,
                    database: database
                )
            } catch {
                Self.logger.debug(
                    "Failed to stamp widget display metadata: \(error.localizedDescription, privacy: .public)"
                )
            }
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

        let host = normalizedHost(fallbackURL: visitURL)
        let title = normalizeDisplayText(titleRaw).ifEmpty(host)

        let descriptionRaw = Self.stringColumn(statement, index: 3)
        let oneLiner = normalizeDisplayText(descriptionRaw ?? "").ifEmpty(host)

        let thumbnailURL = Self.stringColumn(statement, index: 4).flatMap(URL.init(string:))
        let thumbnailDarkURL = Self.stringColumn(statement, index: 5).flatMap(URL.init(string:))

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

    private func normalizedHost(fallbackURL: URL) -> String {
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

    private func openDatabase(path: String) throws -> OpenedDatabase {
        if let readWrite = try open(path: path, flags: SQLITE_OPEN_READWRITE | SQLITE_OPEN_NOMUTEX) {
            return OpenedDatabase(handle: readWrite, canWrite: true)
        }
        let readOnly = try open(path: path, flags: SQLITE_OPEN_READONLY | SQLITE_OPEN_NOMUTEX)
        guard let readOnly else {
            throw NSError(
                domain: "WidgetLocalStore",
                code: Int(SQLITE_CANTOPEN),
                userInfo: [NSLocalizedDescriptionKey: "failed to open cache database"]
            )
        }
        Self.logger.debug("Widget database opened in read-only mode")
        return OpenedDatabase(handle: readOnly, canWrite: false)
    }

    private func open(path: String, flags: Int32) throws -> OpaquePointer? {
        var database: OpaquePointer?
        let openCode = sqlite3_open_v2(path, &database, flags, nil)
        guard openCode == SQLITE_OK, let database else {
            let message = database.flatMap { sqlite3_errmsg($0) }.map { String(cString: $0) } ?? "unknown"
            if let database {
                sqlite3_close(database)
            }
            if (flags & SQLITE_OPEN_READWRITE) != 0 {
                return nil
            }
            throw NSError(
                domain: "WidgetLocalStore",
                code: Int(openCode),
                userInfo: [NSLocalizedDescriptionKey: "failed to open cache database: \(message)"]
            )
        }
        return database
    }

    private func buildSelectSQL(
        configuration: ConfigurationAppIntent,
        prioritizeUnshown: Bool
    ) -> String {
        var filters: [String] = []

        switch configuration.scope {
        case .saved:
            filters.append("COALESCE(discovery_depth, 0) = 0")
        case .discoveredOnly:
            filters.append("COALESCE(discovery_depth, 0) > 0")
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
        let orderPrefix = prioritizeUnshown
            ? "CASE WHEN last_shown_in_widget IS NULL OR last_shown_in_widget <= ? THEN 0 ELSE 1 END ASC, "
            : ""

        return """
            SELECT
                id,
                title,
                url,
                og_description,
                thumbnail_url,
                thumbnail_dark_url
            FROM \(Self.tableName)
            \(whereClause)
            ORDER BY \(orderPrefix)\(orderClause)
            LIMIT ?
        """
    }

    private func updateLastShownInWidget(
        hyperlinkIDs: [Int],
        shownAt: String,
        database: OpaquePointer?
    ) throws {
        guard !hyperlinkIDs.isEmpty else {
            return
        }

        let placeholders = Array(repeating: "?", count: hyperlinkIDs.count).joined(separator: ", ")
        let sql = """
            UPDATE \(Self.tableName)
            SET \(Self.lastShownInWidgetColumn) = ?
            WHERE id IN (\(placeholders))
        """

        var statement: OpaquePointer?
        let prepareCode = sqlite3_prepare_v2(database, sql, -1, &statement, nil)
        guard prepareCode == SQLITE_OK, let statement else {
            let message = String(cString: sqlite3_errmsg(database))
            throw NSError(
                domain: "WidgetLocalStore",
                code: Int(prepareCode),
                userInfo: [NSLocalizedDescriptionKey: "failed to prepare widget stamp update: \(message)"]
            )
        }
        defer { sqlite3_finalize(statement) }

        guard Self.bindText(shownAt, to: statement, index: 1) == SQLITE_OK else {
            throw NSError(
                domain: "WidgetLocalStore",
                code: Int(sqlite3_errcode(database)),
                userInfo: [NSLocalizedDescriptionKey: "failed to bind widget stamp timestamp"]
            )
        }

        for (offset, hyperlinkID) in hyperlinkIDs.enumerated() {
            sqlite3_bind_int(statement, Int32(offset + 2), Int32(hyperlinkID))
        }

        guard sqlite3_step(statement) == SQLITE_DONE else {
            let message = String(cString: sqlite3_errmsg(database))
            throw NSError(
                domain: "WidgetLocalStore",
                code: Int(sqlite3_errcode(database)),
                userInfo: [NSLocalizedDescriptionKey: "failed to update widget stamp rows: \(message)"]
            )
        }
    }

    private static func columnExists(
        _ columnName: String,
        in tableName: String,
        database: OpaquePointer?
    ) -> Bool {
        let sql = """
            SELECT 1
            FROM pragma_table_info('\(tableName)')
            WHERE name = ?
            LIMIT 1
        """

        var statement: OpaquePointer?
        guard sqlite3_prepare_v2(database, sql, -1, &statement, nil) == SQLITE_OK,
              let statement else {
            return false
        }
        defer { sqlite3_finalize(statement) }

        guard bindText(columnName, to: statement, index: 1) == SQLITE_OK else {
            return false
        }

        return sqlite3_step(statement) == SQLITE_ROW
    }

    private static func bindText(_ value: String, to statement: OpaquePointer?, index: Int32) -> Int32 {
        sqlite3_bind_text(statement, index, (value as NSString).utf8String, -1, sqliteTransient)
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
