//
//  Widget.swift
//  Widget
//
//  Created by Pat Nakajima on 2/25/26.
//

import WidgetKit
import SwiftUI
import Foundation
import CryptoKit
import CoreImage
@preconcurrency import LinkPresentation
import UniformTypeIdentifiers
import UIKit

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

private enum WidgetTextNormalizer {
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
                faviconURL: URL(string: "https://blog.rust-lang.org/favicon.ico"),
                thumbnailURL: nil,
                thumbnailDarkURL: nil,
                fallbackColor: nil
            ),
            WidgetHyperlink(
                id: 2,
                title: "SQLite Query Optimizer Notes",
                url: "https://sqlite.org/",
                host: "sqlite.org",
                oneLiner: "Planner behavior and practical indexing strategies.",
                visitURL: URL(string: "https://example.com/hyperlinks/2/visit")!,
                faviconURL: URL(string: "https://sqlite.org/favicon.ico"),
                thumbnailURL: nil,
                thumbnailDarkURL: nil,
                fallbackColor: nil
            ),
            WidgetHyperlink(
                id: 3,
                title: "SwiftUI Widget Layout Guide",
                url: "https://developer.apple.com/",
                host: "developer.apple.com",
                oneLiner: "Widget composition patterns and family-specific layout guidance.",
                visitURL: URL(string: "https://example.com/hyperlinks/3/visit")!,
                faviconURL: URL(string: "https://developer.apple.com/favicon.ico"),
                thumbnailURL: nil,
                thumbnailDarkURL: nil,
                fallbackColor: nil
            ),
            WidgetHyperlink(
                id: 4,
                title: "Production Observability Patterns",
                url: "https://example.com/obs",
                host: "example.com",
                oneLiner: "Metrics, logs, and traces across distributed systems.",
                visitURL: URL(string: "https://example.com/hyperlinks/4/visit")!,
                faviconURL: URL(string: "https://example.com/favicon.ico"),
                thumbnailURL: nil,
                thumbnailDarkURL: nil,
                fallbackColor: nil
            ),
            WidgetHyperlink(
                id: 5,
                title: "Postgres Tuning for Mixed Workloads",
                url: "https://postgresql.org/",
                host: "postgresql.org",
                oneLiner: "Configuration and index tradeoffs for OLTP + analytics.",
                visitURL: URL(string: "https://example.com/hyperlinks/5/visit")!,
                faviconURL: URL(string: "https://postgresql.org/favicon.ico"),
                thumbnailURL: nil,
                thumbnailDarkURL: nil,
                fallbackColor: nil
            ),
            WidgetHyperlink(
                id: 6,
                title: "Build Tooling Notes",
                url: "https://example.com/build",
                host: "example.com",
                oneLiner: "Faster incremental builds and CI cache hygiene.",
                visitURL: URL(string: "https://example.com/hyperlinks/6/visit")!,
                faviconURL: URL(string: "https://example.com/favicon.ico"),
                thumbnailURL: nil,
                thumbnailDarkURL: nil,
                fallbackColor: nil
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

        let baseHyperlinks = responseData.hyperlinks.nodes.map { node in
            let parsedURL = URL(string: node.url)
            let rawHost = parsedURL?.host ?? node.url
            let host = WidgetTextNormalizer.normalizeDisplayText(rawHost)
            let title = WidgetTextNormalizer.normalizeDisplayText(node.title)
            let normalizedDescription = node.ogDescription.map(WidgetTextNormalizer.normalizeDisplayText)
            let oneLiner = Self.oneLiner(ogDescription: normalizedDescription, host: host)
            let thumbnailURL = node.thumbnailUrl.flatMap(URL.init(string:))
            let thumbnailDarkURL = node.thumbnailDarkUrl.flatMap(URL.init(string:))
            let visitURL = baseURL
                .appendingPathComponent("hyperlinks")
                .appendingPathComponent(String(node.id))
                .appendingPathComponent("visit")
            return WidgetHyperlink(
                id: node.id,
                title: title,
                url: node.url,
                host: host,
                oneLiner: oneLiner,
                visitURL: visitURL,
                faviconURL: nil,
                thumbnailURL: thumbnailURL,
                thumbnailDarkURL: thumbnailDarkURL,
                fallbackColor: nil
            )
        }
        return await WidgetVisualResolver.decorate(hyperlinks: baseHyperlinks, session: session)
    }

    private static func oneLiner(ogDescription: String?, host: String) -> String {
        if let description = ogDescription,
           !description.isEmpty {
            return description
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
              thumbnailUrl
              thumbnailDarkUrl
            }
          }
        }
        """
    }
}

private enum FaviconCacheLookup {
    case resolved(URL)
    case miss
    case none
}

private actor WidgetResourceCache {
    static let shared = WidgetResourceCache()

    private static let cacheTTL: TimeInterval = 7 * 24 * 60 * 60
    private static let defaultsKey = "widget_resource_cache_v2"
    private static let cacheDirectoryName = "widget_visual_cache"
    private static let iconsDirectoryName = "icons"

    private struct CacheIndex: Codable {
        var faviconEntries: [String: FaviconCacheEntry] = [:]
        var thumbnailColorEntries: [String: ThumbnailColorEntry] = [:]
    }

    private struct FaviconCacheEntry: Codable {
        let kind: Kind
        let remoteURL: String?
        let localFileName: String?
        let updatedAt: Date

        enum Kind: String, Codable {
            case remote
            case local
            case miss
        }
    }

    private struct ThumbnailColorEntry: Codable {
        let red: Double
        let green: Double
        let blue: Double
        let updatedAt: Date
    }

    private let fileManager = FileManager.default
    private let defaults = UserDefaults(suiteName: WidgetSharedConfig.appGroupID)
    private var hasLoaded = false
    private var index = CacheIndex()

    func cachedFavicon(for key: String) -> FaviconCacheLookup {
        loadIfNeeded()
        guard let entry = index.faviconEntries[key] else {
            return .none
        }
        guard isFresh(entry.updatedAt) else {
            index.faviconEntries.removeValue(forKey: key)
            persist()
            return .none
        }

        switch entry.kind {
        case .remote:
            guard let rawURL = entry.remoteURL,
                  let url = URL(string: rawURL) else {
                return .none
            }
            return .resolved(url)
        case .local:
            guard let fileName = entry.localFileName,
                  let fileURL = iconFileURL(fileName: fileName),
                  fileManager.fileExists(atPath: fileURL.path) else {
                return .none
            }
            return .resolved(fileURL)
        case .miss:
            return .miss
        }
    }

    func storeRemoteFavicon(_ url: URL, for key: String) {
        loadIfNeeded()
        index.faviconEntries[key] = FaviconCacheEntry(
            kind: .remote,
            remoteURL: url.absoluteString,
            localFileName: nil,
            updatedAt: .now
        )
        persist()
    }

    func storeLocalFaviconData(_ data: Data, for key: String) -> URL? {
        loadIfNeeded()
        guard let fileURL = writeIconData(data, key: key) else {
            return nil
        }
        index.faviconEntries[key] = FaviconCacheEntry(
            kind: .local,
            remoteURL: nil,
            localFileName: fileURL.lastPathComponent,
            updatedAt: .now
        )
        persist()
        return fileURL
    }

    func storeFaviconMiss(for key: String) {
        loadIfNeeded()
        index.faviconEntries[key] = FaviconCacheEntry(
            kind: .miss,
            remoteURL: nil,
            localFileName: nil,
            updatedAt: .now
        )
        persist()
    }

    func cachedThumbnailColor(for key: String) -> WidgetRGBColor? {
        loadIfNeeded()
        guard let entry = index.thumbnailColorEntries[key] else {
            return nil
        }
        guard isFresh(entry.updatedAt) else {
            index.thumbnailColorEntries.removeValue(forKey: key)
            persist()
            return nil
        }

        return WidgetRGBColor(
            red: entry.red,
            green: entry.green,
            blue: entry.blue
        )
    }

    func storeThumbnailColor(_ color: WidgetRGBColor, for key: String) {
        loadIfNeeded()
        index.thumbnailColorEntries[key] = ThumbnailColorEntry(
            red: color.red,
            green: color.green,
            blue: color.blue,
            updatedAt: .now
        )
        persist()
    }

    private func loadIfNeeded() {
        guard !hasLoaded else {
            return
        }
        hasLoaded = true

        guard let defaults,
              let data = defaults.data(forKey: Self.defaultsKey),
              let decoded = try? JSONDecoder().decode(CacheIndex.self, from: data) else {
            index = CacheIndex()
            return
        }
        index = decoded
    }

    private func persist() {
        guard let defaults,
              let data = try? JSONEncoder().encode(index) else {
            return
        }
        defaults.set(data, forKey: Self.defaultsKey)
    }

    private func isFresh(_ date: Date) -> Bool {
        Date().timeIntervalSince(date) <= Self.cacheTTL
    }

    private func writeIconData(_ data: Data, key: String) -> URL? {
        guard let fileURL = iconFileURL(fileName: iconFileName(for: key, data: data)) else {
            return nil
        }
        do {
            try data.write(to: fileURL, options: [.atomic])
            return fileURL
        } catch {
            return nil
        }
    }

    private func iconFileURL(fileName: String) -> URL? {
        guard let iconsDirectory = iconsDirectoryURL() else {
            return nil
        }
        return iconsDirectory.appendingPathComponent(fileName, isDirectory: false)
    }

    private func iconsDirectoryURL() -> URL? {
        guard let root = cacheDirectoryURL() else {
            return nil
        }
        let iconsDirectory = root.appendingPathComponent(Self.iconsDirectoryName, isDirectory: true)
        if !fileManager.fileExists(atPath: iconsDirectory.path) {
            do {
                try fileManager.createDirectory(at: iconsDirectory, withIntermediateDirectories: true)
            } catch {
                return nil
            }
        }
        return iconsDirectory
    }

    private func cacheDirectoryURL() -> URL? {
        let baseURL = fileManager
            .containerURL(forSecurityApplicationGroupIdentifier: WidgetSharedConfig.appGroupID)
            ?? fileManager.temporaryDirectory
        let cacheDirectory = baseURL.appendingPathComponent(Self.cacheDirectoryName, isDirectory: true)
        if !fileManager.fileExists(atPath: cacheDirectory.path) {
            do {
                try fileManager.createDirectory(at: cacheDirectory, withIntermediateDirectories: true)
            } catch {
                return nil
            }
        }
        return cacheDirectory
    }

    private func iconFileName(for key: String, data: Data) -> String {
        let hash = Self.sha256Hex(key)
        return "\(hash).\(fileExtension(for: data))"
    }

    private func fileExtension(for data: Data) -> String {
        if data.starts(with: [0x89, 0x50, 0x4E, 0x47]) {
            return "png"
        }
        if data.starts(with: [0xFF, 0xD8, 0xFF]) {
            return "jpg"
        }
        if data.starts(with: [0x47, 0x49, 0x46, 0x38]) {
            return "gif"
        }
        if data.starts(with: [0x00, 0x00, 0x01, 0x00]) {
            return "ico"
        }
        if data.count >= 12,
           data.starts(with: [0x52, 0x49, 0x46, 0x46]),
           Data(data[8..<12]) == Data([0x57, 0x45, 0x42, 0x50]) {
            return "webp"
        }
        return "img"
    }

    private static func sha256Hex(_ value: String) -> String {
        let digest = SHA256.hash(data: Data(value.utf8))
        return digest.map { String(format: "%02x", $0) }.joined()
    }
}

private enum WidgetVisualResolver {
    private static let ciContext = CIContext(options: nil)
    private static let maxFetchedIconBytes = 512_000

    static func decorate(hyperlinks: [WidgetHyperlink], session: URLSession) async -> [WidgetHyperlink] {
        var decorated = [WidgetHyperlink]()
        decorated.reserveCapacity(hyperlinks.count)

        for hyperlink in hyperlinks {
            let faviconURL = await resolveFaviconURL(for: hyperlink, session: session)
            let fallbackColor = faviconURL == nil
                ? await resolveFallbackColor(for: hyperlink, session: session)
                : nil
            decorated.append(
                hyperlink.withVisuals(
                    faviconURL: faviconURL,
                    fallbackColor: fallbackColor
                )
            )
        }

        return decorated
    }

    private static func resolveFaviconURL(for hyperlink: WidgetHyperlink, session: URLSession) async -> URL? {
        guard let rawURL = URL(string: hyperlink.url),
              let pageURL = normalizedPageURL(from: rawURL) else {
            return nil
        }
        let cacheKey = faviconCacheKey(for: pageURL)

        switch await WidgetResourceCache.shared.cachedFavicon(for: cacheKey) {
        case .resolved(let cached):
            return cached
        case .miss:
            return nil
        case .none:
            break
        }

        if let directData = await resolveDirectFaviconData(for: pageURL, session: session),
           let fileURL = await WidgetResourceCache.shared.storeLocalFaviconData(directData, for: cacheKey) {
            return fileURL
        }

        if let lpData = await resolveLPFaviconData(for: pageURL),
           let fileURL = await WidgetResourceCache.shared.storeLocalFaviconData(lpData, for: cacheKey) {
            return fileURL
        }

        await WidgetResourceCache.shared.storeFaviconMiss(for: cacheKey)
        return nil
    }

    private static func resolveDirectFaviconData(for pageURL: URL, session: URLSession) async -> Data? {
        var candidates = await discoverIconCandidates(from: pageURL, session: session)
        if let fallback = originFaviconURL(for: pageURL) {
            candidates.append(fallback)
        }

        for candidate in prioritizeCandidates(dedupeCandidates(candidates)) {
            guard let data = await fetchCandidateIconData(from: candidate, session: session),
                  let normalized = renderableImageData(from: data) else {
                continue
            }
            return normalized
        }
        return nil
    }

    private static func resolveLPFaviconData(for pageURL: URL) async -> Data? {
        guard let metadata = await fetchLinkMetadata(for: pageURL) else {
            return nil
        }
        if let iconProvider = metadata.iconProvider,
           let data = await loadImageData(from: iconProvider),
           let normalized = renderableImageData(from: data) {
            return normalized
        }
        if let imageProvider = metadata.imageProvider,
           let data = await loadImageData(from: imageProvider),
           let normalized = renderableImageData(from: data) {
            return normalized
        }
        return nil
    }

    private static func fetchLinkMetadata(for pageURL: URL) async -> LPLinkMetadata? {
        await withCheckedContinuation { continuation in
            let provider = LPMetadataProvider()
            provider.timeout = 6
            provider.startFetchingMetadata(for: pageURL) { metadata, _ in
                continuation.resume(returning: metadata)
            }
        }
    }

    private static func loadImageData(from provider: NSItemProvider) async -> Data? {
        var typeIdentifiers = provider.registeredTypeIdentifiers.filter { identifier in
            guard let type = UTType(identifier) else {
                return false
            }
            return type.conforms(to: .image)
        }
        if !typeIdentifiers.contains(UTType.image.identifier) {
            typeIdentifiers.append(UTType.image.identifier)
        }

        for typeIdentifier in typeIdentifiers {
            if let data = await provider.loadDataRepresentationAsync(forTypeIdentifier: typeIdentifier),
               !data.isEmpty {
                return data
            }
        }

        if provider.canLoadObject(ofClass: UIImage.self),
           let image = await provider.loadUIImageAsync(),
           let png = image.pngData() {
            return png
        }

        return nil
    }

    private static func discoverIconCandidates(from pageURL: URL, session: URLSession) async -> [URL] {
        guard let html = await fetchHTML(from: pageURL, session: session) else {
            return []
        }
        return extractIconURLs(from: html, baseURL: pageURL)
    }

    private static func fetchHTML(from pageURL: URL, session: URLSession) async -> String? {
        var request = URLRequest(url: pageURL)
        request.httpMethod = "GET"
        request.timeoutInterval = 6
        request.setValue("text/html,application/xhtml+xml", forHTTPHeaderField: "Accept")

        guard let (data, response) = try? await session.data(for: request),
              let http = response as? HTTPURLResponse,
              (200...299).contains(http.statusCode),
              let contentType = http.value(forHTTPHeaderField: "Content-Type")?.lowercased(),
              (contentType.contains("text/html") || contentType.contains("application/xhtml+xml")) else {
            return nil
        }

        let snippet = Data(data.prefix(196_608))
        if let html = String(data: snippet, encoding: .utf8) {
            return html
        }
        return String(decoding: snippet, as: UTF8.self)
    }

    private static func extractIconURLs(from html: String, baseURL: URL) -> [URL] {
        guard let linkRegex = try? NSRegularExpression(pattern: "<link\\b[^>]*>", options: [.caseInsensitive]) else {
            return []
        }

        let range = NSRange(html.startIndex..<html.endIndex, in: html)
        let matches = linkRegex.matches(in: html, options: [], range: range)
        var urls = [URL]()

        for match in matches {
            guard let tagRange = Range(match.range, in: html) else {
                continue
            }
            let tag = String(html[tagRange])
            guard let relValue = attributeValue(named: "rel", in: tag)?.lowercased(),
                  relValue.contains("icon"),
                  let hrefValue = attributeValue(named: "href", in: tag) else {
                continue
            }

            let cleanedHref = hrefValue
                .trimmingCharacters(in: .whitespacesAndNewlines)
                .replacingOccurrences(of: "&amp;", with: "&")
            guard !cleanedHref.isEmpty,
                  let resolved = URL(string: cleanedHref, relativeTo: baseURL)?.absoluteURL,
                  let scheme = resolved.scheme?.lowercased(),
                  scheme == "http" || scheme == "https" else {
                continue
            }
            urls.append(resolved)
        }

        return urls
    }

    private static func attributeValue(named name: String, in tag: String) -> String? {
        let escapedName = NSRegularExpression.escapedPattern(for: name)
        let pattern = "(?i)\\b\(escapedName)\\s*=\\s*(?:\"([^\"]*)\"|'([^']*)'|([^\\s>]+))"
        guard let regex = try? NSRegularExpression(pattern: pattern) else {
            return nil
        }
        let range = NSRange(tag.startIndex..<tag.endIndex, in: tag)
        guard let match = regex.firstMatch(in: tag, options: [], range: range) else {
            return nil
        }

        for group in 1...3 {
            let capture = match.range(at: group)
            if capture.location != NSNotFound,
               let captureRange = Range(capture, in: tag) {
                return String(tag[captureRange])
            }
        }
        return nil
    }

    private static func originFaviconURL(for pageURL: URL) -> URL? {
        guard let scheme = pageURL.scheme,
              let host = pageURL.host else {
            return nil
        }
        var components = URLComponents()
        components.scheme = scheme
        components.host = host
        components.port = pageURL.port
        components.path = "/favicon.ico"
        return components.url
    }

    private static func dedupeCandidates(_ urls: [URL]) -> [URL] {
        var seen = Set<String>()
        var deduped = [URL]()
        deduped.reserveCapacity(urls.count)

        for url in urls {
            guard let scheme = url.scheme?.lowercased(),
                  scheme == "http" || scheme == "https" else {
                continue
            }
            let key = url.absoluteString
            guard seen.insert(key).inserted else {
                continue
            }
            deduped.append(url)
        }

        return deduped
    }

    private static func fetchCandidateIconData(from url: URL, session: URLSession) async -> Data? {
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.timeoutInterval = 6
        request.setValue("image/*,*/*;q=0.8", forHTTPHeaderField: "Accept")

        guard let (data, response) = try? await session.data(for: request),
              let http = response as? HTTPURLResponse,
              (200...299).contains(http.statusCode),
              !data.isEmpty,
              data.count <= maxFetchedIconBytes else {
            return nil
        }

        if let contentType = http.value(forHTTPHeaderField: "Content-Type") {
            guard isImageContentType(contentType) || looksLikeImageData(data) else {
                return nil
            }
            return data
        }

        return looksLikeImageData(data) ? data : nil
    }

    private static func renderableImageData(from data: Data) -> Data? {
        guard let image = UIImage(data: data),
              let pngData = image.pngData(),
              !pngData.isEmpty else {
            return nil
        }
        return pngData
    }

    private static func prioritizeCandidates(_ urls: [URL]) -> [URL] {
        urls.enumerated()
            .sorted { lhs, rhs in
                let leftPriority = candidatePriority(for: lhs.element)
                let rightPriority = candidatePriority(for: rhs.element)
                if leftPriority != rightPriority {
                    return leftPriority < rightPriority
                }
                return lhs.offset < rhs.offset
            }
            .map(\.element)
    }

    private static func candidatePriority(for url: URL) -> Int {
        let path = url.path.lowercased()
        if path.contains("apple-touch-icon")
            || path.hasSuffix(".png")
            || path.hasSuffix(".jpg")
            || path.hasSuffix(".jpeg")
            || path.hasSuffix(".gif")
            || path.hasSuffix(".webp") {
            return 0
        }
        if path.hasSuffix(".ico") || path == "/favicon.ico" {
            return 2
        }
        return 1
    }

    private static func isImageContentType(_ contentType: String) -> Bool {
        let normalized = contentType.lowercased()
        return normalized.contains("image/")
            || normalized.contains("image/svg+xml")
            || normalized.contains("application/octet-stream")
    }

    private static func looksLikeImageData(_ data: Data) -> Bool {
        if data.starts(with: [0x89, 0x50, 0x4E, 0x47]) {
            return true
        }
        if data.starts(with: [0xFF, 0xD8, 0xFF]) {
            return true
        }
        if data.starts(with: [0x47, 0x49, 0x46, 0x38]) {
            return true
        }
        if data.starts(with: [0x00, 0x00, 0x01, 0x00]) {
            return true
        }
        if data.count >= 12,
           data.starts(with: [0x52, 0x49, 0x46, 0x46]),
           Data(data[8..<12]) == Data([0x57, 0x45, 0x42, 0x50]) {
            return true
        }
        let prefix = String(decoding: data.prefix(32), as: UTF8.self).lowercased()
        return prefix.contains("<svg")
    }

    private static func resolveFallbackColor(for hyperlink: WidgetHyperlink, session: URLSession) async -> WidgetRGBColor? {
        guard let thumbnailURL = hyperlink.thumbnailURL ?? hyperlink.thumbnailDarkURL else {
            return nil
        }
        let cacheKey = thumbnailURL.absoluteString

        if let cached = await WidgetResourceCache.shared.cachedThumbnailColor(for: cacheKey) {
            return cached
        }

        guard let imageData = await fetchImageData(from: thumbnailURL, session: session),
              let sampledColor = sampledColor(from: imageData) else {
            return nil
        }

        await WidgetResourceCache.shared.storeThumbnailColor(sampledColor, for: cacheKey)
        return sampledColor
    }

    private static func fetchImageData(from url: URL, session: URLSession) async -> Data? {
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.timeoutInterval = 8
        request.setValue("image/*,*/*;q=0.8", forHTTPHeaderField: "Accept")

        guard let (data, response) = try? await session.data(for: request),
              let http = response as? HTTPURLResponse,
              (200...299).contains(http.statusCode),
              !data.isEmpty else {
            return nil
        }
        return data
    }

    private static func sampledColor(from imageData: Data) -> WidgetRGBColor? {
        guard let image = UIImage(data: imageData),
              let cgImage = image.cgImage else {
            return nil
        }

        let ciImage = CIImage(cgImage: cgImage)
        let extent = ciImage.extent
        guard !extent.isEmpty,
              let filter = CIFilter(name: "CIAreaAverage") else {
            return nil
        }

        filter.setValue(ciImage, forKey: kCIInputImageKey)
        filter.setValue(CIVector(cgRect: extent), forKey: kCIInputExtentKey)
        guard let outputImage = filter.outputImage else {
            return nil
        }

        var bitmap = [UInt8](repeating: 0, count: 4)
        let bounds = CGRect(x: 0, y: 0, width: 1, height: 1)
        ciContext.render(
            outputImage,
            toBitmap: &bitmap,
            rowBytes: 4,
            bounds: bounds,
            format: .RGBA8,
            colorSpace: CGColorSpaceCreateDeviceRGB()
        )

        let alpha = Double(bitmap[3]) / 255.0
        guard alpha > 0.01 else {
            return nil
        }

        let red = (Double(bitmap[0]) / 255.0) * alpha + (1.0 - alpha)
        let green = (Double(bitmap[1]) / 255.0) * alpha + (1.0 - alpha)
        let blue = (Double(bitmap[2]) / 255.0) * alpha + (1.0 - alpha)
        return WidgetRGBColor(red: red, green: green, blue: blue)
    }

    private static func normalizedPageURL(from url: URL) -> URL? {
        guard var components = URLComponents(url: url, resolvingAgainstBaseURL: true),
              let scheme = components.scheme?.lowercased(),
              (scheme == "http" || scheme == "https"),
              let host = components.host,
              !host.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return nil
        }
        components.user = nil
        components.password = nil
        return components.url
    }

    private static func faviconCacheKey(for pageURL: URL) -> String {
        var components = URLComponents()
        components.scheme = pageURL.scheme?.lowercased()
        components.host = pageURL.host?.lowercased()
        components.port = pageURL.port
        return components.string ?? pageURL.absoluteString
    }
}

private extension NSItemProvider {
    func loadDataRepresentationAsync(forTypeIdentifier typeIdentifier: String) async -> Data? {
        await withCheckedContinuation { continuation in
            loadDataRepresentation(forTypeIdentifier: typeIdentifier) { data, _ in
                continuation.resume(returning: data)
            }
        }
    }

    func loadUIImageAsync() async -> UIImage? {
        await withCheckedContinuation { continuation in
            loadObject(ofClass: UIImage.self) { object, _ in
                continuation.resume(returning: object as? UIImage)
            }
        }
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
    let thumbnailUrl: String?
    let thumbnailDarkUrl: String?
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
                Spacer(minLength: 0)

                ForEach(Array(links.enumerated()), id: \.element.id) { index, hyperlink in
                    Link(destination: WidgetTapURLBuilder.destinationURL(for: hyperlink.visitURL)) {
                        HStack(alignment: .firstTextBaseline, spacing: 8) {
                            WidgetFaviconView(hyperlink: hyperlink, size: 16)
                                .alignmentGuide(.firstTextBaseline) { dimensions in
                                    dimensions[VerticalAlignment.center]
                                }
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
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
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
            .fill(hyperlink.fallbackColor?.swiftUIColor ?? Self.hostColor(for: hyperlink.host))
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
