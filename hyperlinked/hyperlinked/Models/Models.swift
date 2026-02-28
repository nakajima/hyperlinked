import Foundation

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

struct Hyperlink: Codable, Equatable, Hashable, Identifiable {
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
