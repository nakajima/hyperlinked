//
//  Widget.swift
//  Widget
//
//  Created by Pat Nakajima on 2/25/26.
//

import WidgetKit
import SwiftUI
import Foundation

private enum WidgetSharedConfig {
    static let appGroupID = "group.fm.folder.hyperlinked"
    static let selectedServerURLKey = "selected_server_base_url"

    static func selectedServerURL() -> URL? {
        guard let defaults = UserDefaults(suiteName: appGroupID),
              let rawValue = defaults.string(forKey: selectedServerURLKey) else {
            return nil
        }
        return normalizedServerURL(from: rawValue)
    }

    private static func normalizedServerURL(from rawValue: String) -> URL? {
        let trimmed = rawValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            return nil
        }

        let candidate = trimmed.contains("://") ? trimmed : "http://\(trimmed)"
        guard var components = URLComponents(string: candidate),
              let scheme = components.scheme?.lowercased(),
              (scheme == "http" || scheme == "https"),
              let host = components.host,
              !host.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return nil
        }

        components.user = nil
        components.password = nil
        components.path = ""
        components.query = nil
        components.fragment = nil

        guard let url = components.url else {
            return nil
        }

        let absolute = url.absoluteString
        if absolute.hasSuffix("/") {
            return URL(string: String(absolute.dropLast()))
        }
        return url
    }
}

private enum WidgetQueryBuilder {
    static func build(for configuration: ConfigurationAppIntent) -> String {
        var tokens = [
            "scope:\(configuration.scope.queryToken)",
            "order:\(configuration.sortOrder.queryToken)",
        ]

        if configuration.unclickedOnly {
            tokens.append("clicks:unclicked")
        }

        return tokens.joined(separator: " ")
    }
}

private enum WidgetTapURLBuilder {
    static func destinationURL(for visitURL: URL) -> URL {
        var components = URLComponents()
        components.scheme = "hyperlinked"
        components.host = "widget"
        components.path = "/visit"
        components.queryItems = [
            URLQueryItem(name: "target", value: visitURL.absoluteString),
        ]
        return components.url ?? visitURL
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
            hyperlinks: sampleHyperlinks,
            status: .loaded
        )
    }

    static func preview(configuration: ConfigurationAppIntent) -> HyperlinksEntry {
        HyperlinksEntry(
            date: .now,
            configuration: configuration,
            hyperlinks: sampleHyperlinks,
            status: .loaded
        )
    }

    private static var sampleHyperlinks: [WidgetHyperlink] {
        [
            WidgetHyperlink(
                id: 1,
                title: "Rust 2025 Roadmap and Language Team Priorities",
                url: "https://blog.rust-lang.org/",
                host: "blog.rust-lang.org",
                oneLiner: "Language updates, ergonomics work, and compiler priorities.",
                visitURL: URL(string: "https://example.com/hyperlinks/1/visit")!,
                faviconURL: URL(string: "https://blog.rust-lang.org/favicon.ico")
            ),
            WidgetHyperlink(
                id: 2,
                title: "SQLite Query Optimizer Notes",
                url: "https://sqlite.org/",
                host: "sqlite.org",
                oneLiner: "Planner behavior and practical indexing strategies.",
                visitURL: URL(string: "https://example.com/hyperlinks/2/visit")!,
                faviconURL: URL(string: "https://sqlite.org/favicon.ico")
            ),
            WidgetHyperlink(
                id: 3,
                title: "SwiftUI Widget Layout Guide",
                url: "https://developer.apple.com/",
                host: "developer.apple.com",
                oneLiner: "Widget composition patterns and family-specific layout guidance.",
                visitURL: URL(string: "https://example.com/hyperlinks/3/visit")!,
                faviconURL: URL(string: "https://developer.apple.com/favicon.ico")
            ),
            WidgetHyperlink(
                id: 4,
                title: "Production Observability Patterns",
                url: "https://example.com/obs",
                host: "example.com",
                oneLiner: "Metrics, logs, and traces across distributed systems.",
                visitURL: URL(string: "https://example.com/hyperlinks/4/visit")!,
                faviconURL: URL(string: "https://example.com/favicon.ico")
            ),
            WidgetHyperlink(
                id: 5,
                title: "Postgres Tuning for Mixed Workloads",
                url: "https://postgresql.org/",
                host: "postgresql.org",
                oneLiner: "Configuration and index tradeoffs for OLTP + analytics.",
                visitURL: URL(string: "https://example.com/hyperlinks/5/visit")!,
                faviconURL: URL(string: "https://postgresql.org/favicon.ico")
            ),
            WidgetHyperlink(
                id: 6,
                title: "Build Tooling Notes",
                url: "https://example.com/build",
                host: "example.com",
                oneLiner: "Faster incremental builds and CI cache hygiene.",
                visitURL: URL(string: "https://example.com/hyperlinks/6/visit")!,
                faviconURL: URL(string: "https://example.com/favicon.ico")
            ),
        ]
    }
}

struct HyperlinksProvider: AppIntentTimelineProvider {
    private static let refreshInterval: TimeInterval = 30 * 60
    private static let maxHyperlinks = 6

    func placeholder(in context: Context) -> HyperlinksEntry {
        .placeholder
    }

    func snapshot(for configuration: ConfigurationAppIntent, in context: Context) async -> HyperlinksEntry {
        if context.isPreview {
            return .preview(configuration: configuration)
        }
        return await Self.loadEntry(configuration: configuration)
    }

    func timeline(for configuration: ConfigurationAppIntent, in context: Context) async -> Timeline<HyperlinksEntry> {
        let entry = await Self.loadEntry(configuration: configuration)
        return Timeline(
            entries: [entry],
            policy: .after(Date().addingTimeInterval(Self.refreshInterval))
        )
    }

    private static func loadEntry(configuration: ConfigurationAppIntent) async -> HyperlinksEntry {
        guard let baseURL = WidgetSharedConfig.selectedServerURL() else {
            return .noServer(configuration: configuration)
        }

        do {
            let query = WidgetQueryBuilder.build(for: configuration)
            let client = WidgetAPIClient(baseURL: baseURL)
            let hyperlinks = try await client.listHyperlinks(q: query, limit: maxHyperlinks)
            if hyperlinks.isEmpty {
                return .empty(configuration: configuration)
            }

            return HyperlinksEntry(
                date: .now,
                configuration: configuration,
                hyperlinks: hyperlinks,
                status: .loaded
            )
        } catch {
            return .error(configuration: configuration)
        }
    }
}

private enum WidgetAPIClientError: Error {
    case invalidResponse
    case unexpectedStatus
    case missingData
    case graphql(String)
}

private struct WidgetAPIClient {
    let baseURL: URL
    let session: URLSession

    init(baseURL: URL, session: URLSession = .shared) {
        self.baseURL = baseURL
        self.session = session
    }

    func listHyperlinks(q: String, limit: Int) async throws -> [WidgetHyperlink] {
        var request = URLRequest(url: baseURL.appendingPathComponent("graphql"))
        request.httpMethod = "POST"
        request.timeoutInterval = 15
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")

        let payload = GraphQLRequestPayload(
            query: Self.hyperlinksQuery(limit: limit),
            variables: ["q": q]
        )
        request.httpBody = try JSONEncoder().encode(payload)

        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else {
            throw WidgetAPIClientError.invalidResponse
        }
        guard (200...299).contains(http.statusCode) else {
            throw WidgetAPIClientError.unexpectedStatus
        }

        let decoded = try JSONDecoder().decode(GraphQLResponsePayload<GraphQLHyperlinksPayload>.self, from: data)
        if let message = decoded.errors?.first?.message,
           !message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            throw WidgetAPIClientError.graphql(message)
        }

        guard let responseData = decoded.data else {
            throw WidgetAPIClientError.missingData
        }

        return responseData.hyperlinks.nodes.map { node in
            let parsedURL = URL(string: node.url)
            let host = parsedURL?.host ?? node.url
            let faviconURL = Self.faviconURL(for: parsedURL, fallbackHost: host)
            let oneLiner = Self.oneLiner(ogDescription: node.ogDescription, host: host)
            let visitURL = baseURL
                .appendingPathComponent("hyperlinks")
                .appendingPathComponent(String(node.id))
                .appendingPathComponent("visit")
            return WidgetHyperlink(
                id: node.id,
                title: node.title,
                url: node.url,
                host: host,
                oneLiner: oneLiner,
                visitURL: visitURL,
                faviconURL: faviconURL
            )
        }
    }

    private static func faviconURL(for parsedURL: URL?, fallbackHost: String) -> URL? {
        if let parsedURL,
           let scheme = parsedURL.scheme,
           let host = parsedURL.host {
            var components = URLComponents()
            components.scheme = scheme
            components.host = host
            components.path = "/favicon.ico"
            return components.url
        }

        let trimmedHost = fallbackHost.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedHost.isEmpty,
              let encodedHost = trimmedHost.addingPercentEncoding(withAllowedCharacters: .urlHostAllowed),
              let url = URL(string: "https://\(encodedHost)/favicon.ico") else {
            return nil
        }
        return url
    }

    private static func oneLiner(ogDescription: String?, host: String) -> String {
        let normalized = ogDescription?
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .replacingOccurrences(of: "\n", with: " ")
        if let normalized,
           !normalized.isEmpty {
            return normalized
        }
        return host
    }

    private static func hyperlinksQuery(limit: Int) -> String {
        """
        query WidgetHyperlinks($q: String) {
          hyperlinks(
            q: $q
            pagination: { page: { limit: \(limit), page: 0 } }
          ) {
            nodes {
              id
              title
              url
              ogDescription
            }
          }
        }
        """
    }
}

private struct GraphQLRequestPayload: Encodable {
    let query: String
    let variables: [String: String]?
}

private struct GraphQLResponsePayload<T: Decodable>: Decodable {
    let data: T?
    let errors: [GraphQLErrorPayload]?
}

private struct GraphQLErrorPayload: Decodable {
    let message: String
}

private struct GraphQLHyperlinksPayload: Decodable {
    let hyperlinks: GraphQLHyperlinksConnectionPayload
}

private struct GraphQLHyperlinksConnectionPayload: Decodable {
    let nodes: [GraphQLHyperlinkNodePayload]
}

private struct GraphQLHyperlinkNodePayload: Decodable {
    let id: Int
    let title: String
    let url: String
    let ogDescription: String?
}

private struct HyperlinksWidgetEntryView: View {
    @Environment(\.widgetFamily) private var widgetFamily

    let entry: HyperlinksEntry

    var body: some View {
        switch entry.status {
        case .loaded:
            loadedView
        case .noServer:
            messageView(
                title: "No Server Selected",
                subtitle: "Open hyperlinked and choose a server URL."
            )
        case .empty:
            messageView(
                title: "No Matching Links",
                subtitle: "Try changing widget options."
            )
        case .error:
            messageView(
                title: "Couldn’t Refresh",
                subtitle: "Widget will retry automatically."
            )
        }
    }

    @ViewBuilder
    private var loadedView: some View {
        let links = Array(entry.hyperlinks.prefix(rowLimit))

        if links.isEmpty {
            messageView(
                title: "No Links",
                subtitle: "Add a link in the app."
            )
        } else if widgetFamily == .systemSmall {
            let first = links[0]
            Link(destination: WidgetTapURLBuilder.destinationURL(for: first.visitURL)) {
                VStack(alignment: .leading, spacing: 6) {
                    HStack(spacing: 6) {
                        WidgetFaviconView(hyperlink: first, size: 14)
                        Text(first.host)
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                    Text(first.title)
                        .font(.headline)
                        .multilineTextAlignment(.leading)
                        .lineLimit(3)
                    Spacer(minLength: 0)
                    Text(first.oneLiner)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            }
            .buttonStyle(.plain)
        } else {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(Array(links.enumerated()), id: \.element.id) { index, hyperlink in
                    Link(destination: WidgetTapURLBuilder.destinationURL(for: hyperlink.visitURL)) {
                        HStack(alignment: .top, spacing: 8) {
                            WidgetFaviconView(hyperlink: hyperlink, size: 16)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(hyperlink.title)
                                    .font(.subheadline)
                                    .multilineTextAlignment(.leading)
                                    .lineLimit(1)
                                Text(hyperlink.oneLiner)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                    .lineLimit(1)
                            }
                            Spacer(minLength: 0)
                        }
                    }
                    .buttonStyle(.plain)

                    if index < links.count - 1 {
                        Divider()
                    }
                }

                Spacer(minLength: 0)
            }
        }
    }

    private func messageView(title: String, subtitle: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title)
                .font(.headline)
                .multilineTextAlignment(.leading)
                .lineLimit(2)
            Text(subtitle)
                .font(.caption)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.leading)
                .lineLimit(4)
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private var rowLimit: Int {
        switch widgetFamily {
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

private struct WidgetFaviconView: View {
    let hyperlink: WidgetHyperlink
    let size: CGFloat

    var body: some View {
        Group {
            if let faviconURL = hyperlink.faviconURL {
                AsyncImage(url: faviconURL) { phase in
                    switch phase {
                    case .success(let image):
                        image
                            .resizable()
                            .scaledToFill()
                    default:
                        fallbackDot
                    }
                }
            } else {
                fallbackDot
            }
        }
        .frame(width: size, height: size)
        .clipShape(Circle())
        .overlay(Circle().stroke(Color.primary.opacity(0.12), lineWidth: 0.5))
    }

    private var fallbackDot: some View {
        Circle()
            .fill(Self.hostColor(for: hyperlink.host))
    }

    private static func hostColor(for host: String) -> Color {
        let normalized = host.lowercased()
        let hash = normalized.unicodeScalars.reduce(0) { partial, scalar in
            (partial &* 33 &+ Int(scalar.value)) & 0x7fffffff
        }
        let hue = Double(hash % 360) / 360.0
        return Color(hue: hue, saturation: 0.72, brightness: 0.84)
    }
}

struct HyperlinksWidget: Widget {
    private let kind = "HyperlinksWidget"

    var body: some WidgetConfiguration {
        AppIntentConfiguration(
            kind: kind,
            intent: ConfigurationAppIntent.self,
            provider: HyperlinksProvider()
        ) { entry in
            HyperlinksWidgetEntryView(entry: entry)
                .containerBackground(.fill.tertiary, for: .widget)
        }
        .configurationDisplayName("Hyperlinks")
        .description("Browse links from your server directly on your Home Screen.")
        .supportedFamilies([.systemSmall, .systemMedium, .systemLarge])
    }
}

#Preview(as: .systemSmall) {
    HyperlinksWidget()
} timeline: {
    HyperlinksEntry.preview(configuration: .previewNewestRoot)
}

#Preview(as: .systemMedium) {
    HyperlinksWidget()
} timeline: {
    HyperlinksEntry.preview(configuration: .previewRandomAllUnclicked)
}

#Preview(as: .systemLarge) {
    HyperlinksWidget()
} timeline: {
    HyperlinksEntry.preview(configuration: .previewNewestRoot)
}
