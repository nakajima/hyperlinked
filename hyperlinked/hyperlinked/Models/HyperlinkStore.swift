import Foundation
import GRDB
import GRDBQuery

final class HyperlinkStore {
    private static let tableName = DB.hyperlinkTableName
    private static let maxSummaryLength = 120

    private let dbQueue: DatabaseQueue

    private init(dbQueue: DatabaseQueue) {
        self.dbQueue = dbQueue
    }

    static func openShared() throws -> HyperlinkStore {
        return HyperlinkStore(dbQueue: try DB.databaseQueue())
    }

    func upsert(hyperlinks: [Hyperlink]) throws {
        guard !hyperlinks.isEmpty else {
            return
        }

        try dbQueue.write { db in
            for hyperlink in hyperlinks {
                guard let row = Self.persistedRow(from: hyperlink) else {
                    continue
                }

                try db.execute(
                    sql: """
                        INSERT INTO \(Self.tableName) (
                            id,
                            title,
                            url,
                            host,
                            one_liner,
                            clicks_count,
                            last_clicked_at,
                            discovery_depth,
                            created_at,
                            updated_at,
                            thumbnail_url,
                            thumbnail_dark_url
                        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                        ON CONFLICT(id) DO UPDATE SET
                            title = excluded.title,
                            url = excluded.url,
                            host = excluded.host,
                            one_liner = excluded.one_liner,
                            clicks_count = excluded.clicks_count,
                            last_clicked_at = excluded.last_clicked_at,
                            discovery_depth = excluded.discovery_depth,
                            created_at = excluded.created_at,
                            updated_at = excluded.updated_at,
                            thumbnail_url = excluded.thumbnail_url,
                            thumbnail_dark_url = excluded.thumbnail_dark_url
                    """,
                    arguments: [
                        row.id,
                        row.title,
                        row.url,
                        row.host,
                        row.oneLiner,
                        row.clicksCount,
                        row.lastClickedAt,
                        row.discoveryDepth,
                        row.createdAt,
                        row.updatedAt,
                        row.thumbnailURL,
                        row.thumbnailDarkURL,
                    ]
                )
            }
        }
    }

    func upsert(hyperlink: Hyperlink) throws {
        try upsert(hyperlinks: [hyperlink])
    }

    func apply(updatedBatch: UpdatedHyperlinksBatch) throws {
        try dbQueue.write { db in
            for change in updatedBatch.changes {
                switch change.changeType {
                case .updated:
                    guard let hyperlink = change.hyperlink,
                          let row = Self.persistedRow(from: hyperlink) else {
                        continue
                    }

                    try db.execute(
                        sql: """
                            INSERT INTO \(Self.tableName) (
                                id,
                                title,
                                url,
                                host,
                                one_liner,
                                clicks_count,
                                last_clicked_at,
                                discovery_depth,
                                created_at,
                                updated_at,
                                thumbnail_url,
                                thumbnail_dark_url
                            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                            ON CONFLICT(id) DO UPDATE SET
                                title = excluded.title,
                                url = excluded.url,
                                host = excluded.host,
                                one_liner = excluded.one_liner,
                                clicks_count = excluded.clicks_count,
                                last_clicked_at = excluded.last_clicked_at,
                                discovery_depth = excluded.discovery_depth,
                                created_at = excluded.created_at,
                                updated_at = excluded.updated_at,
                                thumbnail_url = excluded.thumbnail_url,
                                thumbnail_dark_url = excluded.thumbnail_dark_url
                        """,
                        arguments: [
                            row.id,
                            row.title,
                            row.url,
                            row.host,
                            row.oneLiner,
                            row.clicksCount,
                            row.lastClickedAt,
                            row.discoveryDepth,
                            row.createdAt,
                            row.updatedAt,
                            row.thumbnailURL,
                            row.thumbnailDarkURL,
                        ]
                    )
                case .deleted:
                    try db.execute(
                        sql: "DELETE FROM \(Self.tableName) WHERE id = ?",
                        arguments: [change.id]
                    )
                }
            }
        }
    }

    private struct PersistedRow {
        let id: Int
        let title: String
        let url: String
        let host: String
        let oneLiner: String
        let clicksCount: Int
        let lastClickedAt: String?
        let discoveryDepth: Int
        let createdAt: String
        let updatedAt: String
        let thumbnailURL: String?
        let thumbnailDarkURL: String?
    }

    private static func persistedRow(from hyperlink: Hyperlink) -> PersistedRow? {
        guard let url = URL(string: hyperlink.url),
              let host = normalizedHost(from: url) else {
            return nil
        }

        let normalizedTitle = normalizeDisplayText(hyperlink.title)
        let oneLiner = summaryLine(ogDescription: hyperlink.ogDescription, host: host)
        return PersistedRow(
            id: hyperlink.id,
            title: normalizedTitle.isEmpty ? hyperlink.url : normalizedTitle,
            url: hyperlink.url,
            host: host,
            oneLiner: oneLiner,
            clicksCount: hyperlink.clicksCount,
            lastClickedAt: hyperlink.lastClickedAt?.trimmingCharacters(in: .whitespacesAndNewlines),
            discoveryDepth: hyperlink.discoveryDepth ?? 0,
            createdAt: hyperlink.createdAt,
            updatedAt: hyperlink.updatedAt,
            thumbnailURL: hyperlink.thumbnailURL?.trimmingCharacters(in: .whitespacesAndNewlines).nilIfEmpty,
            thumbnailDarkURL: hyperlink.thumbnailDarkURL?.trimmingCharacters(in: .whitespacesAndNewlines).nilIfEmpty
        )
    }

    private static func normalizedHost(from url: URL) -> String? {
        guard let host = url.host?.lowercased(),
              !host.isEmpty else {
            return nil
        }

        if host.hasPrefix("www.") {
            return String(host.dropFirst(4))
        }
        return host
    }

    private static func summaryLine(ogDescription: String?, host: String) -> String {
        guard let ogDescription else {
            return host
        }

        let normalized = normalizeDisplayText(ogDescription)
        guard !normalized.isEmpty else {
            return host
        }

        guard normalized.count > maxSummaryLength else {
            return normalized
        }

        let cutoff = normalized.index(normalized.startIndex, offsetBy: maxSummaryLength - 3)
        return String(normalized[..<cutoff]).trimmingCharacters(in: .whitespacesAndNewlines) + "..."
    }

    private static func normalizeDisplayText(_ value: String) -> String {
        guard !value.isEmpty else {
            return ""
        }

        let collapsed = value.replacingOccurrences(
            of: #"\s+"#,
            with: " ",
            options: .regularExpression
        )
        return collapsed.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

private struct CachedHyperlinkRow: FetchableRecord, Decodable {
    let id: Int
    let title: String
    let url: String
    let host: String
    let clicksCount: Int
    let lastClickedAt: String?
    let discoveryDepth: Int
    let createdAt: String
    let updatedAt: String
    let thumbnailURL: String?
    let thumbnailDarkURL: String?

    enum CodingKeys: String, CodingKey {
        case id
        case title
        case url
        case host
        case clicksCount = "clicks_count"
        case lastClickedAt = "last_clicked_at"
        case discoveryDepth = "discovery_depth"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case thumbnailURL = "thumbnail_url"
        case thumbnailDarkURL = "thumbnail_dark_url"
    }

    func toHyperlink() -> Hyperlink {
        Hyperlink(
            id: id,
            title: title,
            url: url,
            rawURL: url,
            ogDescription: nil,
            discoveryDepth: discoveryDepth,
            clicksCount: clicksCount,
            lastClickedAt: lastClickedAt,
            processingState: "ready",
            createdAt: createdAt,
            updatedAt: updatedAt,
            thumbnailURL: thumbnailURL,
            thumbnailDarkURL: thumbnailDarkURL,
            screenshotURL: nil,
            screenshotDarkURL: nil
        )
    }
}

struct CachedHyperlinksRequest: ValueObservationQueryable {
    static let defaultValue: [Hyperlink] = []

    let limit: Int

    func fetch(_ db: Database) throws -> [Hyperlink] {
        let rows = try CachedHyperlinkRow.fetchAll(
            db,
            sql: """
                SELECT
                    id,
                    title,
                    url,
                    host,
                    clicks_count,
                    last_clicked_at,
                    discovery_depth,
                    created_at,
                    updated_at,
                    thumbnail_url,
                    thumbnail_dark_url
                FROM \(DB.hyperlinkTableName)
                ORDER BY created_at DESC, id DESC
                LIMIT ?
            """,
            arguments: [limit]
        )
        return rows.map { $0.toHyperlink() }
    }
}

private extension String {
    var nilIfEmpty: String? {
        isEmpty ? nil : self
    }
}
