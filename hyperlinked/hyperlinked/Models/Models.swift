import Foundation

struct Hyperlink: Codable, Equatable, Hashable, Identifiable {
    let id: Int
    let title: String
    let url: String
    let rawURL: String
    let clicksCount: Int
    let lastClickedAt: String?
    let processingState: String
    let createdAt: String
    let updatedAt: String
    let thumbnailURL: String?
    let thumbnailDarkURL: String?
    let screenshotURL: String?
    let screenshotDarkURL: String?

    enum CodingKeys: String, CodingKey {
        case id
        case title
        case url
        case rawURL = "raw_url"
        case clicksCount = "clicks_count"
        case lastClickedAt = "last_clicked_at"
        case processingState = "processing_state"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case thumbnailURL = "thumbnail_url"
        case thumbnailDarkURL = "thumbnail_dark_url"
        case screenshotURL = "screenshot_url"
        case screenshotDarkURL = "screenshot_dark_url"
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
