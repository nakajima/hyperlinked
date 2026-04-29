//
//  Widget.swift
//  Widget
//
//  Created by Pat Nakajima on 2/25/26.
//

import WidgetKit
import SwiftUI
import Foundation
import OSLog

enum WidgetTapURLBuilder {
    static func destinationURL(for hyperlink: WidgetHyperlink) -> URL {
        var components = URLComponents()
        components.scheme = "hyperlinked"
        components.host = "widget"
        components.path = "/visit"
        components.queryItems = [
            URLQueryItem(name: "target", value: hyperlink.visitURL.absoluteString),
            URLQueryItem(name: "id", value: String(hyperlink.id)),
        ]
        return components.url ?? hyperlink.visitURL
    }
}

enum WidgetTextNormalizer {
    static func normalizeDisplayText(_ value: String) -> String {
        guard !value.isEmpty else {
            return ""
        }

        let decoded = decodeHTMLEntities(value)
        let collapsed = decoded.replacingOccurrences(
            of: #"\s+"#,
            with: " ",
            options: .regularExpression
        )
        return collapsed.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private static func decodeHTMLEntities(_ value: String) -> String {
        guard value.contains("&") else {
            return value
        }

        var decoded = String()
        decoded.reserveCapacity(value.count)
        var cursor = value.startIndex

        while cursor < value.endIndex {
            let character = value[cursor]
            guard character == "&",
                  let semicolon = value[cursor...].firstIndex(of: ";"),
                  semicolon > value.index(after: cursor) else {
                decoded.append(character)
                cursor = value.index(after: cursor)
                continue
            }

            let entityStart = value.index(after: cursor)
            let entity = String(value[entityStart..<semicolon])
            if let resolved = decodeEntity(entity) {
                decoded.append(resolved)
                cursor = value.index(after: semicolon)
            } else {
                decoded.append(character)
                cursor = value.index(after: cursor)
            }
        }

        return decoded
    }

    private static func decodeEntity(_ entity: String) -> String? {
        if let numeric = decodeNumericEntity(entity) {
            return numeric
        }

        switch entity.lowercased() {
        case "amp":
            return "&"
        case "lt":
            return "<"
        case "gt":
            return ">"
        case "quot":
            return "\""
        case "apos":
            return "'"
        case "nbsp":
            return " "
        default:
            return nil
        }
    }

    private static func decodeNumericEntity(_ entity: String) -> String? {
        let scalarValue: UInt32
        if entity.hasPrefix("#x") || entity.hasPrefix("#X") {
            let digits = String(entity.dropFirst(2))
            guard let parsed = UInt32(digits, radix: 16) else {
                return nil
            }
            scalarValue = parsed
        } else if entity.hasPrefix("#") {
            let digits = String(entity.dropFirst())
            guard let parsed = UInt32(digits) else {
                return nil
            }
            scalarValue = parsed
        } else {
            return nil
        }

        guard let scalar = UnicodeScalar(scalarValue) else {
            return nil
        }
        return String(Character(scalar))
    }
}

enum WidgetDiagnostics {
    static let favicon = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "fm.folder.hyperlinked.Widget",
        category: "favicon"
    )
    static let cache = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "fm.folder.hyperlinked.Widget",
        category: "local-cache"
    )

    static func sanitizedURL(_ url: URL) -> String {
        if url.isFileURL {
            return "file://\(url.lastPathComponent)"
        }

        let host = url.host?.lowercased() ?? "unknown-host"
        let path = url.path.isEmpty ? "/" : url.path
        return "\(host)\(path)"
    }
}

struct WidgetHyperlink: Identifiable {
    let id: Int
    let title: String
    let url: String
    let host: String
    let oneLiner: String
    let visitURL: URL
    let faviconURL: URL?
    let thumbnailURL: URL?
    let thumbnailDarkURL: URL?
    let fallbackColor: WidgetRGBColor?

    func withVisuals(faviconURL: URL?, fallbackColor: WidgetRGBColor?) -> WidgetHyperlink {
        WidgetHyperlink(
            id: id,
            title: title,
            url: url,
            host: host,
            oneLiner: oneLiner,
            visitURL: visitURL,
            faviconURL: faviconURL,
            thumbnailURL: thumbnailURL,
            thumbnailDarkURL: thumbnailDarkURL,
            fallbackColor: fallbackColor
        )
    }
}

struct WidgetRGBColor: Codable {
    let red: Double
    let green: Double
    let blue: Double

    var swiftUIColor: Color {
        Color(red: red, green: green, blue: blue)
    }
}

enum EntryStatus {
    case loaded
    case noServer
    case empty
    case error
}

struct HyperlinksEntry: TimelineEntry {
    let date: Date
    let configuration: ConfigurationAppIntent
    let hyperlinks: [WidgetHyperlink]
    let status: EntryStatus
    let rotationStampStatus: WidgetRotationStampStatus

    init(
        date: Date,
        configuration: ConfigurationAppIntent,
        hyperlinks: [WidgetHyperlink],
        status: EntryStatus,
        rotationStampStatus: WidgetRotationStampStatus = .healthy
    ) {
        self.date = date
        self.configuration = configuration
        self.hyperlinks = hyperlinks
        self.status = status
        self.rotationStampStatus = rotationStampStatus
    }

    static func noServer(configuration: ConfigurationAppIntent) -> HyperlinksEntry {
        HyperlinksEntry(
            date: .now,
            configuration: configuration,
            hyperlinks: [],
            status: .noServer
        )
    }

    static func empty(configuration: ConfigurationAppIntent) -> HyperlinksEntry {
        HyperlinksEntry(
            date: .now,
            configuration: configuration,
            hyperlinks: [],
            status: .empty
        )
    }

    static func error(configuration: ConfigurationAppIntent) -> HyperlinksEntry {
        HyperlinksEntry(
            date: .now,
            configuration: configuration,
            hyperlinks: [],
            status: .error
        )
    }

    static var placeholder: HyperlinksEntry {
        HyperlinksEntry(
            date: .now,
            configuration: .previewNewestRoot,
            hyperlinks: WidgetPreviewData.hyperlinks(for: .recent),
            status: .loaded
        )
    }

    static func preview(
        configuration: ConfigurationAppIntent,
        dataset: WidgetPreviewDataset = .recent
    ) -> HyperlinksEntry {
        HyperlinksEntry(
            date: .now,
            configuration: configuration,
            hyperlinks: WidgetPreviewData.hyperlinks(for: dataset),
            status: .loaded
        )
    }
}

struct HyperlinksProvider: AppIntentTimelineProvider {
    private static let refreshInterval: TimeInterval = 20 * 60
    private static let timelineEntryCount = 6

    func placeholder(in context: Context) -> HyperlinksEntry {
        .placeholder
    }

    func snapshot(for configuration: ConfigurationAppIntent, in context: Context) async -> HyperlinksEntry {
        if context.isPreview {
            return .preview(configuration: configuration)
        }
        return await Self.loadSingleEntry(configuration: configuration, family: context.family)
    }

    func timeline(for configuration: ConfigurationAppIntent, in context: Context) async -> Timeline<HyperlinksEntry> {
        let entries = await Self.loadTimelineEntries(configuration: configuration, family: context.family)
        let refreshDate = (entries.last?.date ?? .now).addingTimeInterval(Self.refreshInterval)
        return Timeline(
            entries: entries,
            policy: .after(refreshDate)
        )
    }

    private static func loadSingleEntry(
        configuration: ConfigurationAppIntent,
        family: WidgetFamily
    ) async -> HyperlinksEntry {
        do {
            let localStore = WidgetLocalStore()
            let displayLimit = limit(for: family)
            let baseHyperlinks = try localStore.listHyperlinks(
                configuration: configuration,
                limit: displayLimit,
                recordShown: configuration.rotateSlowly
            )
            if baseHyperlinks.isEmpty {
                return .empty(configuration: configuration)
            }

            let hyperlinks = await WidgetVisualResolver.decorate(
                hyperlinks: baseHyperlinks,
                session: .shared
            )
            let rotationStampStatus: WidgetRotationStampStatus = {
                guard configuration.rotateSlowly else {
                    return .healthy
                }
                return WidgetDiagnosticsBridge.rotationStampStatus()
            }()
            return HyperlinksEntry(
                date: .now,
                configuration: configuration,
                hyperlinks: hyperlinks,
                status: .loaded,
                rotationStampStatus: rotationStampStatus
            )
        } catch {
            WidgetDiagnostics.cache.debug(
                "Failed to load widget links from local cache: \(error.localizedDescription, privacy: .public)"
            )
            return .error(configuration: configuration)
        }
    }

    private static func loadTimelineEntries(
        configuration: ConfigurationAppIntent,
        family: WidgetFamily
    ) async -> [HyperlinksEntry] {
        guard configuration.rotateSlowly else {
            return [await loadSingleEntry(configuration: configuration, family: family)]
        }

        do {
            let localStore = WidgetLocalStore()
            let displayLimit = limit(for: family)
            let candidateLimit = max(displayLimit, displayLimit * Self.timelineEntryCount)
            let baseHyperlinks = try localStore.listHyperlinks(
                configuration: configuration,
                limit: candidateLimit,
                recordShown: false
            )
            guard !baseHyperlinks.isEmpty else {
                return [.empty(configuration: configuration)]
            }

            let decorated = await WidgetVisualResolver.decorate(
                hyperlinks: baseHyperlinks,
                session: .shared
            )
            let startDate = Date()
            let entryDates = (0..<Self.timelineEntryCount).map {
                startDate.addingTimeInterval(Double($0) * Self.refreshInterval)
            }
            let plannedHyperlinkSets = plannedHyperlinkSets(
                from: decorated,
                displayLimit: displayLimit,
                entryCount: entryDates.count
            )

            if let firstHyperlinks = plannedHyperlinkSets.first {
                localStore.recordShownHyperlinks(firstHyperlinks.map(\.id), at: entryDates[0])
            }

            let rotationStampStatus = WidgetDiagnosticsBridge.rotationStampStatus()
            return zip(entryDates, plannedHyperlinkSets).map { entryDate, hyperlinks in
                HyperlinksEntry(
                    date: entryDate,
                    configuration: configuration,
                    hyperlinks: hyperlinks,
                    status: .loaded,
                    rotationStampStatus: rotationStampStatus
                )
            }
        } catch {
            WidgetDiagnostics.cache.debug(
                "Failed to build rotating widget timeline: \(error.localizedDescription, privacy: .public)"
            )
            return [.error(configuration: configuration)]
        }
    }

    private static func plannedHyperlinkSets(
        from hyperlinks: [WidgetHyperlink],
        displayLimit: Int,
        entryCount: Int
    ) -> [[WidgetHyperlink]] {
        guard !hyperlinks.isEmpty else {
            return []
        }

        let windowSize = min(displayLimit, hyperlinks.count)
        let step = max(1, windowSize)

        return (0..<max(1, entryCount)).map { entryIndex in
            let offset = (entryIndex * step) % hyperlinks.count
            return rotatedWindow(
                from: hyperlinks,
                offset: offset,
                limit: windowSize
            )
        }
    }

    private static func rotatedWindow(
        from hyperlinks: [WidgetHyperlink],
        offset: Int,
        limit: Int
    ) -> [WidgetHyperlink] {
        guard !hyperlinks.isEmpty else {
            return []
        }

        let windowSize = min(limit, hyperlinks.count)
        return (0..<windowSize).map { index in
            hyperlinks[(offset + index) % hyperlinks.count]
        }
    }

    private static func limit(for family: WidgetFamily) -> Int {
        switch family {
        case .systemSmall:
            return 1
        case .systemMedium:
            return 3
        case .systemLarge:
            return 6
        default:
            return 3
        }
    }
}
