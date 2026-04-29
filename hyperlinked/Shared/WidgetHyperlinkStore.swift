import Foundation
import GRDB

struct WidgetSelectionConfig {
    enum Scope {
        case saved
        case discoveredOnly
        case all
    }

    enum SortOrder {
        case newest
        case oldest
        case random
    }

    let scope: Scope
    let sortOrder: SortOrder
    let unclickedOnly: Bool
}

struct WidgetHyperlinkRecord: Sendable {
    let id: Int
    let title: String
    let url: String
    let host: String
    let oneLiner: String
    let thumbnailURL: String?
    let thumbnailDarkURL: String?
}

final class WidgetHyperlinkStore {
    private let dbQueue: DatabaseQueue
    private let shownAtFormatter: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        formatter.timeZone = TimeZone(secondsFromGMT: 0)
        return formatter
    }()

    init(dbQueue: DatabaseQueue) {
        self.dbQueue = dbQueue
    }

    static func openShared() throws -> WidgetHyperlinkStore {
        WidgetHyperlinkStore(dbQueue: try DB.databaseQueue())
    }

    func listHyperlinks(configuration: WidgetSelectionConfig, limit: Int) throws -> [WidgetHyperlinkRecord] {
        guard limit > 0 else {
            return []
        }

        return try dbQueue.read { db in
            let sql = buildSelectSQL(configuration: configuration)
            var arguments: StatementArguments = [limit]
            if configuration.unclickedOnly {
                arguments = [0, limit]
            }

            return try Row.fetchAll(db, sql: sql, arguments: arguments).compactMap(Self.record(from:))
        }
    }

    func recordShownHyperlinks(_ hyperlinkIDs: [Int], at shownAt: Date = .now) throws {
        guard !hyperlinkIDs.isEmpty else {
            return
        }

        let shownAtText = shownAtFormatter.string(from: shownAt)
        try dbQueue.write { db in
            let placeholders = Array(repeating: "?", count: hyperlinkIDs.count).joined(separator: ",")
            var arguments = StatementArguments([shownAtText])
            for hyperlinkID in hyperlinkIDs {
                arguments += [hyperlinkID]
            }

            try db.execute(
                sql: """
                    UPDATE \(DB.hyperlinkTableName)
                    SET last_shown_in_widget = ?
                    WHERE id IN (\(placeholders))
                """,
                arguments: arguments
            )
        }
    }

    private func buildSelectSQL(configuration: WidgetSelectionConfig) -> String {
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
            filters.append("clicks_count = ?")
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
                summary,
                og_description,
                thumbnail_url,
                thumbnail_dark_url
            FROM \(DB.hyperlinkTableName)
            \(whereClause)
            ORDER BY \(orderClause)
            LIMIT ?
        """
    }

    private static func record(from row: Row) -> WidgetHyperlinkRecord? {
        guard let urlRaw: String = row["url"],
              let visitURL = URL(string: urlRaw) else {
            return nil
        }

        let host = normalizedHost(fallbackURL: visitURL)
        let titleRaw: String = row["title"]
        let title = normalizeDisplayText(titleRaw).ifEmpty(host)

        let summary: String? = row["summary"]
        let ogDescription: String? = row["og_description"]
        let oneLiner = normalizeDisplayText(summary ?? ogDescription ?? "").ifEmpty(host)

        return WidgetHyperlinkRecord(
            id: row["id"],
            title: title,
            url: urlRaw,
            host: host,
            oneLiner: oneLiner,
            thumbnailURL: row["thumbnail_url"],
            thumbnailDarkURL: row["thumbnail_dark_url"]
        )
    }

    private static func normalizedHost(fallbackURL: URL) -> String {
        let fallback = fallbackURL.host?.lowercased() ?? fallbackURL.absoluteString
        if fallback.hasPrefix("www.") {
            return String(fallback.dropFirst(4))
        }
        return fallback
    }

    private static func normalizeDisplayText(_ value: String) -> String {
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
