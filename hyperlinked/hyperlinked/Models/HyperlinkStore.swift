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
                try Self.persistedRow(from: hyperlink).upsert(db)
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
                    guard let hyperlink = change.hyperlink else {
                        continue
                    }
                    try Self.persistedRow(from: hyperlink).upsert(db)
                case .deleted:
                    _ = try HyperlinkRow.deleteOne(db, key: change.id)
                }
            }
        }
    }

    func clearAll() throws {
        try dbQueue.write { db in
            _ = try HyperlinkRow.deleteAll(db)
        }
    }

    fileprivate struct HyperlinkRow: Codable, FetchableRecord, PersistableRecord, TableRecord {
        static let databaseTableName = DB.hyperlinkTableName

        let id: Int
        let title: String
        let url: String
        let host: String
        let oneLiner: String
        let isURLValid: Bool
        let clicksCount: Int
        let lastClickedAt: String?
        let discoveryDepth: Int
        let createdAt: String
        let updatedAt: String
        let thumbnailURL: String?
        let thumbnailDarkURL: String?
        let discoveredViaJSON: String

        enum CodingKeys: String, CodingKey, ColumnExpression {
            case id
            case title
            case url
            case host
            case oneLiner = "one_liner"
            case isURLValid = "is_url_valid"
            case clicksCount = "clicks_count"
            case lastClickedAt = "last_clicked_at"
            case discoveryDepth = "discovery_depth"
            case createdAt = "created_at"
            case updatedAt = "updated_at"
            case thumbnailURL = "thumbnail_url"
            case thumbnailDarkURL = "thumbnail_dark_url"
            case discoveredViaJSON = "discovered_via_json"
        }

        typealias Columns = CodingKeys

        func toHyperlink() -> Hyperlink {
            Hyperlink(
                id: id,
                title: title,
                url: url,
                rawURL: url,
                ogDescription: nil,
                isURLValid: isURLValid,
                discoveryDepth: discoveryDepth,
                clicksCount: clicksCount,
                lastClickedAt: lastClickedAt,
                processingState: "ready",
                createdAt: createdAt,
                updatedAt: updatedAt,
                thumbnailURL: thumbnailURL,
                thumbnailDarkURL: thumbnailDarkURL,
                screenshotURL: nil,
                screenshotDarkURL: nil,
                discoveredVia: decodeDiscoveredViaJSON(discoveredViaJSON)
            )
        }
    }

    private static func persistedRow(from hyperlink: Hyperlink) -> HyperlinkRow {
        let canonicalURL = hyperlink.url
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .nilIfEmpty
            ?? hyperlink.rawURL.trimmingCharacters(in: .whitespacesAndNewlines)
        let parsedURL = URL(string: canonicalURL)
        let isURLValid = hyperlink.isURLValid ?? isValidWebURL(parsedURL)
        let host = normalizedHost(from: parsedURL) ?? fallbackHost(from: canonicalURL)

        let normalizedTitle = normalizeDisplayText(hyperlink.title)
        let oneLiner = summaryLine(ogDescription: hyperlink.ogDescription, host: host)
        return HyperlinkRow(
            id: hyperlink.id,
            title: normalizedTitle.isEmpty ? canonicalURL : normalizedTitle,
            url: canonicalURL,
            host: host,
            oneLiner: oneLiner,
            isURLValid: isURLValid,
            clicksCount: hyperlink.clicksCount,
            lastClickedAt: hyperlink.lastClickedAt?.trimmingCharacters(in: .whitespacesAndNewlines),
            discoveryDepth: hyperlink.discoveryDepth ?? 0,
            createdAt: hyperlink.createdAt,
            updatedAt: hyperlink.updatedAt,
            thumbnailURL: hyperlink.thumbnailURL?.trimmingCharacters(in: .whitespacesAndNewlines).nilIfEmpty,
            thumbnailDarkURL: hyperlink.thumbnailDarkURL?.trimmingCharacters(in: .whitespacesAndNewlines).nilIfEmpty,
            discoveredViaJSON: encodeDiscoveredVia(hyperlink.discoveredVia)
        )
    }

    private static func encodeDiscoveredVia(_ discoveredVia: [HyperlinkRef]) -> String {
        guard let data = try? JSONEncoder().encode(discoveredVia),
              let json = String(data: data, encoding: .utf8) else {
            return "[]"
        }
        return json
    }

    private static func normalizedHost(from url: URL?) -> String? {
        guard let host = url?.host else {
            return nil
        }
        return normalizedHostValue(host)
    }

    private static func normalizedHostValue(_ host: String) -> String? {
        let lowered = host
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased()
        guard !lowered.isEmpty else {
            return nil
        }

        let withoutWWW = lowered.hasPrefix("www.") ? String(lowered.dropFirst(4)) : lowered
        let withoutPort = withoutWWW.split(separator: ":").first.map(String.init) ?? withoutWWW
        let sanitized = withoutPort.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !sanitized.isEmpty else {
            return nil
        }
        return sanitized
    }

    private static func isValidWebURL(_ url: URL?) -> Bool {
        guard let url,
              let scheme = url.scheme?.lowercased(),
              (scheme == "http" || scheme == "https"),
              let host = normalizedHost(from: url),
              !host.isEmpty else {
            return false
        }
        return true
    }

    private static func fallbackHost(from rawURL: String) -> String {
        let trimmed = rawURL.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            return "invalid-url"
        }

        if let host = normalizedHostValue(URLComponents(string: trimmed)?.host ?? "") {
            return host
        }

        if let host = normalizedHostValue(URLComponents(string: "https://\(trimmed)")?.host ?? "") {
            return host
        }

        let head = trimmed.split(whereSeparator: { "/?#".contains($0) }).first.map(String.init)
        if let head,
           let host = normalizedHostValue(head) {
            return host
        }

        return "invalid-url"
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

struct CachedHyperlinksRequest: ValueObservationQueryable {
    static let defaultValue: [Hyperlink] = []

    let limit: Int
    let rootOnly: Bool

    func fetch(_ db: Database) throws -> [Hyperlink] {
        var sql = """
            SELECT
                id,
                title,
                url,
                is_url_valid,
                clicks_count,
                last_clicked_at,
                discovery_depth,
                created_at,
                updated_at,
                thumbnail_url,
                thumbnail_dark_url,
                discovered_via_json
            FROM \(DB.hyperlinkTableName)
        """

        if rootOnly {
            sql += "\nWHERE discovery_depth = 0"
        }

        sql += """

            ORDER BY created_at DESC, id DESC
            LIMIT ?
            """

        let rows = try Row.fetchAll(db, sql: sql, arguments: [limit])
        return rows.map(Hyperlink.init(cachedRow:))
    }
}

private extension Hyperlink {
    nonisolated init(cachedRow row: Row) {
        let discoveredViaJSON: String = row["discovered_via_json"]
        self.init(
            id: row["id"],
            title: row["title"],
            url: row["url"],
            rawURL: row["url"],
            ogDescription: nil,
            isURLValid: row["is_url_valid"],
            discoveryDepth: row["discovery_depth"],
            clicksCount: row["clicks_count"],
            lastClickedAt: row["last_clicked_at"],
            processingState: "ready",
            createdAt: row["created_at"],
            updatedAt: row["updated_at"],
            thumbnailURL: row["thumbnail_url"],
            thumbnailDarkURL: row["thumbnail_dark_url"],
            screenshotURL: nil,
            screenshotDarkURL: nil,
            discoveredVia: decodeDiscoveredViaJSON(discoveredViaJSON)
        )
    }
}

private nonisolated func decodeDiscoveredViaJSON(_ json: String) -> [HyperlinkRef] {
    let data = Data(json.utf8)
    guard let decoded = try? JSONDecoder().decode([HyperlinkRef].self, from: data) else {
        return []
    }
    return decoded
}

private extension String {
    var nilIfEmpty: String? {
        isEmpty ? nil : self
    }
}
