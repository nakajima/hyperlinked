import Foundation
import GRDB

struct HyperlinkRef: Codable, Equatable, Hashable, Identifiable {
    let id: Int
    let title: String
    let url: String
    let rawURL: String

    enum CodingKeys: String, CodingKey {
        case id
        case title
        case url
        case rawURL = "raw_url"
    }
}

struct Hyperlink: Decodable, Equatable, Hashable, Identifiable {
    let id: Int
    let title: String
    let url: String
    let rawURL: String
    let ogDescription: String?
    let isURLValid: Bool?
    let discoveryDepth: Int?
    let clicksCount: Int
    let lastClickedAt: String?
    let processingState: String
    let createdAt: String
    let updatedAt: String
    let thumbnailURL: String?
    let thumbnailDarkURL: String?
    let screenshotURL: String?
    let screenshotDarkURL: String?
    let discoveredVia: [HyperlinkRef]

    enum CodingKeys: String, CodingKey {
        case id
        case title
        case url
        case rawURL = "raw_url"
        case ogDescription = "og_description"
        case isURLValid = "is_url_valid"
        case discoveryDepth = "discovery_depth"
        case clicksCount = "clicks_count"
        case lastClickedAt = "last_clicked_at"
        case processingState = "processing_state"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case thumbnailURL = "thumbnail_url"
        case thumbnailDarkURL = "thumbnail_dark_url"
        case screenshotURL = "screenshot_url"
        case screenshotDarkURL = "screenshot_dark_url"
        case discoveredVia = "discovered_via"
    }

    nonisolated init(
        id: Int,
        title: String,
        url: String,
        rawURL: String,
        ogDescription: String?,
        isURLValid: Bool?,
        discoveryDepth: Int?,
        clicksCount: Int,
        lastClickedAt: String?,
        processingState: String,
        createdAt: String,
        updatedAt: String,
        thumbnailURL: String?,
        thumbnailDarkURL: String?,
        screenshotURL: String?,
        screenshotDarkURL: String?,
        discoveredVia: [HyperlinkRef] = []
    ) {
        self.id = id
        self.title = title
        self.url = url
        self.rawURL = rawURL
        self.ogDescription = ogDescription
        self.isURLValid = isURLValid
        self.discoveryDepth = discoveryDepth
        self.clicksCount = clicksCount
        self.lastClickedAt = lastClickedAt
        self.processingState = processingState
        self.createdAt = createdAt
        self.updatedAt = updatedAt
        self.thumbnailURL = thumbnailURL
        self.thumbnailDarkURL = thumbnailDarkURL
        self.screenshotURL = screenshotURL
        self.screenshotDarkURL = screenshotDarkURL
        self.discoveredVia = discoveredVia
    }

    nonisolated init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(Int.self, forKey: .id)
        title = try container.decode(String.self, forKey: .title)
        url = try container.decode(String.self, forKey: .url)
        rawURL = try container.decode(String.self, forKey: .rawURL)
        ogDescription = try container.decodeIfPresent(String.self, forKey: .ogDescription)
        isURLValid = try container.decodeIfPresent(Bool.self, forKey: .isURLValid)
        discoveryDepth = try container.decodeIfPresent(Int.self, forKey: .discoveryDepth)
        clicksCount = try container.decode(Int.self, forKey: .clicksCount)
        lastClickedAt = try container.decodeIfPresent(String.self, forKey: .lastClickedAt)
        processingState = try container.decode(String.self, forKey: .processingState)
        createdAt = try container.decode(String.self, forKey: .createdAt)
        updatedAt = try container.decode(String.self, forKey: .updatedAt)
        thumbnailURL = try container.decodeIfPresent(String.self, forKey: .thumbnailURL)
        thumbnailDarkURL = try container.decodeIfPresent(String.self, forKey: .thumbnailDarkURL)
        screenshotURL = try container.decodeIfPresent(String.self, forKey: .screenshotURL)
        screenshotDarkURL = try container.decodeIfPresent(String.self, forKey: .screenshotDarkURL)
        discoveredVia = try container.decodeIfPresent([HyperlinkRef].self, forKey: .discoveredVia) ?? []
    }

}

extension Hyperlink: FetchableRecord, PersistableRecord, TableRecord {
    nonisolated static let databaseTableName = DB.hyperlinkTableName

    enum Columns: String, ColumnExpression {
        case id
        case title
        case url
        case rawURL = "raw_url"
        case ogDescription = "og_description"
        case isURLValid = "is_url_valid"
        case discoveryDepth = "discovery_depth"
        case clicksCount = "clicks_count"
        case lastClickedAt = "last_clicked_at"
        case processingState = "processing_state"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case thumbnailURL = "thumbnail_url"
        case thumbnailDarkURL = "thumbnail_dark_url"
        case screenshotURL = "screenshot_url"
        case screenshotDarkURL = "screenshot_dark_url"
        case discoveredViaJSON = "discovered_via_json"
    }

    nonisolated init(row: Row) {
        let url: String = row[Columns.url]
        let rawURL: String
        if row.hasColumn("raw_url"), let value: String = row[Columns.rawURL] {
            rawURL = value
        } else {
            rawURL = url
        }

        let processingState: String
        if row.hasColumn("processing_state"),
           let value: String = row[Columns.processingState] {
            processingState = value
        } else {
            processingState = "ready"
        }

        let discoveredViaJSON: String
        if row.hasColumn("discovered_via_json"),
           let value: String = row[Columns.discoveredViaJSON] {
            discoveredViaJSON = value
        } else {
            discoveredViaJSON = "[]"
        }

        self.init(
            id: row[Columns.id],
            title: row[Columns.title],
            url: url,
            rawURL: rawURL,
            ogDescription: row[Columns.ogDescription],
            isURLValid: row[Columns.isURLValid],
            discoveryDepth: row[Columns.discoveryDepth],
            clicksCount: row[Columns.clicksCount],
            lastClickedAt: row[Columns.lastClickedAt],
            processingState: processingState,
            createdAt: row[Columns.createdAt],
            updatedAt: row[Columns.updatedAt],
            thumbnailURL: row[Columns.thumbnailURL],
            thumbnailDarkURL: row[Columns.thumbnailDarkURL],
            screenshotURL: row[Columns.screenshotURL],
            screenshotDarkURL: row[Columns.screenshotDarkURL],
            discoveredVia: Self.decodeDiscoveredViaDatabaseJSON(discoveredViaJSON)
        )
    }

    nonisolated func encode(to container: inout PersistenceContainer) {
        container[Columns.id] = id
        container[Columns.title] = title
        container[Columns.url] = url
        container[Columns.rawURL] = rawURL
        container[Columns.ogDescription] = ogDescription
        container[Columns.isURLValid] = isURLValid
        container[Columns.discoveryDepth] = discoveryDepth
        container[Columns.clicksCount] = clicksCount
        container[Columns.lastClickedAt] = lastClickedAt
        container[Columns.processingState] = processingState
        container[Columns.createdAt] = createdAt
        container[Columns.updatedAt] = updatedAt
        container[Columns.thumbnailURL] = thumbnailURL
        container[Columns.thumbnailDarkURL] = thumbnailDarkURL
        container[Columns.screenshotURL] = screenshotURL
        container[Columns.screenshotDarkURL] = screenshotDarkURL
        container[Columns.discoveredViaJSON] = Self.encodeDiscoveredViaDatabaseJSON(discoveredVia)
    }

    nonisolated private static func encodeDiscoveredViaDatabaseJSON(_ discoveredVia: [HyperlinkRef]) -> String {
        guard let data = try? JSONEncoder().encode(discoveredVia),
              let json = String(data: data, encoding: .utf8) else {
            return "[]"
        }
        return json
    }

    nonisolated private static func decodeDiscoveredViaDatabaseJSON(_ json: String) -> [HyperlinkRef] {
        let data = Data(json.utf8)
        guard let decoded = try? JSONDecoder().decode([HyperlinkRef].self, from: data) else {
            return []
        }
        return decoded
    }
}

struct HyperlinksIndexResponse: Decodable {
    let items: [Hyperlink]
}

struct HyperlinkInput: Encodable {
    let title: String
    let url: String
}

struct APIErrorResponse: Decodable {
    let error: String
}

struct DiscoveredServer: Hashable, Identifiable {
    let id: String
    let name: String
    let host: String
    let port: Int

    var displayAddress: String {
        "\(host):\(port)"
    }

    var baseURL: URL? {
        var components = URLComponents()
        components.scheme = "http"
        components.host = host
        components.port = port
        return components.url
    }
}
