import Foundation

enum WidgetPreviewDataset {
    case recent
    case mixed
    case sparseDescriptions
}

enum WidgetPreviewData {
    private static let maxOneLinerLength = 92

    private struct Record {
        let id: Int
        let title: String
        let url: String
        let ogDescription: String?
    }

    private static let records: [Record] = [
        Record(
            id: 2159,
            title: "nakajima/LiveModelDemo",
            url: "https://github.com/nakajima/LiveModelDemo",
            ogDescription: "Contribute to nakajima/LiveModelDemo development by creating an account on GitHub."
        ),
        Record(
            id: 2158,
            title: "Blackbird/Sources/Blackbird/BlackbirdSwiftUI.swift at 076827d5be06c3a1cf686b2012e8f3853cba7b38 - marcoarment/Blackbird",
            url: "https://github.com/marcoarment/Blackbird/blob/076827d5be06c3a1cf686b2012e8f3853cba7b38/Sources/Blackbird/BlackbirdSwiftUI.swift",
            ogDescription: "Contribute to marcoarment/Blackbird development by creating an account on GitHub."
        ),
        Record(
            id: 2157,
            title: "SwiftDataKit - Unleashing Advanced Core Data Features in SwiftData",
            url: "https://fatbobman.com/en/posts/use-core-data-features-in-swiftdata-by-swiftdatakit/",
            ogDescription: "Explore how to use Core Data's advanced features in SwiftData with SwiftDataKit, bypassing the Core Data stack."
        ),
        Record(
            id: 2156,
            title: "Par Part 1: Sequent Calculus",
            url: "https://ryanbrewer.dev/posts/sequent-calculus/",
            ogDescription: nil
        ),
        Record(
            id: 2155,
            title: "Frama by Pangram Pangram - A Precise Geometric Typeface",
            url: "https://pp-frama.com",
            ogDescription: "Frama is a geometric typeface by Pangram Pangram, built for clarity and structure."
        ),
        Record(
            id: 2154,
            title: "The HoTT Book",
            url: "https://homotopytypetheory.org/book/",
            ogDescription: "Homotopy Type Theory: Univalent Foundations of Mathematics."
        ),
        Record(
            id: 2153,
            title: "cargo publish - The Cargo Book",
            url: "https://doc.rust-lang.org/cargo/commands/cargo-publish.html",
            ogDescription: nil
        ),
    ]

    static func hyperlinks(for dataset: WidgetPreviewDataset) -> [WidgetHyperlink] {
        selectedRecords(for: dataset).compactMap(hyperlink(from:))
    }

    private static func selectedRecords(for dataset: WidgetPreviewDataset) -> [Record] {
        switch dataset {
        case .recent:
            return Array(records.prefix(4))
        case .mixed:
            return records
        case .sparseDescriptions:
            return records.filter { ($0.ogDescription ?? "").trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
        }
    }

    private static func hyperlink(from record: Record) -> WidgetHyperlink? {
        guard let pageURL = URL(string: record.url),
              let host = normalizedHost(from: pageURL) else {
            return nil
        }

        return WidgetHyperlink(
            id: record.id,
            title: WidgetTextNormalizer.normalizeDisplayText(record.title),
            url: record.url,
            host: host,
            oneLiner: oneLiner(ogDescription: record.ogDescription, host: host),
            visitURL: previewVisitURL(for: record.url),
            faviconURL: previewFaviconURL(for: pageURL),
            thumbnailURL: nil,
            thumbnailDarkURL: nil,
            fallbackColor: nil
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

    private static func previewVisitURL(for urlString: String) -> URL {
        URL(string: urlString) ?? URL(string: "https://example.com")!
    }

    private static func previewFaviconURL(for pageURL: URL) -> URL? {
        guard let host = pageURL.host?.lowercased() else {
            return nil
        }
        var components = URLComponents()
        components.scheme = "https"
        components.host = host
        components.path = "/favicon.ico"
        return components.url
    }

    private static func oneLiner(ogDescription: String?, host: String) -> String {
        guard let ogDescription else {
            return host
        }

        let normalized = WidgetTextNormalizer.normalizeDisplayText(ogDescription)
        guard !normalized.isEmpty else {
            return host
        }

        guard normalized.count > maxOneLinerLength else {
            return normalized
        }
        let cutoff = normalized.index(normalized.startIndex, offsetBy: maxOneLinerLength - 3)
        return String(normalized[..<cutoff]).trimmingCharacters(in: .whitespacesAndNewlines) + "..."
    }
}
