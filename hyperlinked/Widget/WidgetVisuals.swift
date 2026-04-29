import CryptoKit
import CoreImage
import Foundation
import OSLog
@preconcurrency import LinkPresentation
import UniformTypeIdentifiers
import UIKit

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
        let localFileName: String?
        let updatedAt: Date
        let isMiss: Bool
    }

    private struct ThumbnailColorEntry: Codable {
        let red: Double
        let green: Double
        let blue: Double
        let updatedAt: Date
    }

    private let fileManager = FileManager.default
    private let defaults = UserDefaults(suiteName: AppGroupConfig.appGroupID)
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

        if entry.isMiss {
            return .miss
        }

        guard let fileName = entry.localFileName,
              let fileURL = iconFileURL(fileName: fileName),
              fileManager.fileExists(atPath: fileURL.path),
              fileManager.isReadableFile(atPath: fileURL.path) else {
            index.faviconEntries.removeValue(forKey: key)
            persist()
            return .none
        }
        return .resolved(fileURL)
    }

    func storeLocalFaviconData(_ data: Data, for key: String) -> URL? {
        loadIfNeeded()
        guard let fileURL = writeIconData(data, key: key) else {
            return nil
        }
        index.faviconEntries[key] = FaviconCacheEntry(
            localFileName: fileURL.lastPathComponent,
            updatedAt: .now,
            isMiss: false
        )
        persist()
        return fileURL
    }

    func storeFaviconMiss(for key: String) {
        loadIfNeeded()
        index.faviconEntries[key] = FaviconCacheEntry(
            localFileName: nil,
            updatedAt: .now,
            isMiss: true
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
        return ensuredDirectoryURL(
            root.appendingPathComponent(Self.iconsDirectoryName, isDirectory: true)
        )
    }

    private func cacheDirectoryURL() -> URL? {
        let baseURL = fileManager
            .containerURL(forSecurityApplicationGroupIdentifier: AppGroupConfig.appGroupID)
            ?? fileManager.temporaryDirectory
        return ensuredDirectoryURL(
            baseURL.appendingPathComponent(Self.cacheDirectoryName, isDirectory: true)
        )
    }

    private func ensuredDirectoryURL(_ url: URL) -> URL? {
        do {
            try fileManager.createDirectory(at: url, withIntermediateDirectories: true)
            return url
        } catch {
            return nil
        }
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

enum WidgetVisualResolver {
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
            WidgetDiagnostics.favicon.debug(
                "Skipping favicon resolution for invalid URL: \(hyperlink.url, privacy: .public)"
            )
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

        WidgetDiagnostics.favicon.debug(
            "No favicon resolved for \(WidgetDiagnostics.sanitizedURL(pageURL), privacy: .public); falling back"
        )
        await WidgetResourceCache.shared.storeFaviconMiss(for: cacheKey)
        return nil
    }

    private static func resolveDirectFaviconData(for pageURL: URL, session: URLSession) async -> Data? {
        var candidates = await discoverIconCandidates(from: pageURL, session: session)
        if let fallback = originFaviconURL(for: pageURL) {
            candidates.append(fallback)
        }

        for candidate in prioritizeCandidates(dedupeCandidates(candidates)) {
            guard let data = await fetchCandidateIconData(from: candidate, session: session) else {
                continue
            }
            guard let normalized = renderableImageData(from: data) else {
                WidgetDiagnostics.favicon.debug(
                    "Discarding favicon candidate with undecodable image \(WidgetDiagnostics.sanitizedURL(candidate), privacy: .public)"
                )
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
        for provider in [metadata.iconProvider, metadata.imageProvider] {
            guard let provider,
                  let data = await loadImageData(from: provider),
                  let normalized = renderableImageData(from: data) else {
                continue
            }
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

        return String(decoding: data.prefix(196_608), as: UTF8.self)
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
              let http = response as? HTTPURLResponse else {
            WidgetDiagnostics.favicon.debug(
                "Favicon fetch failed for \(WidgetDiagnostics.sanitizedURL(url), privacy: .public): request error"
            )
            return nil
        }

        guard (200...299).contains(http.statusCode) else {
            WidgetDiagnostics.favicon.debug(
                "Favicon fetch rejected for \(WidgetDiagnostics.sanitizedURL(url), privacy: .public): status \(http.statusCode, privacy: .public)"
            )
            return nil
        }

        guard !data.isEmpty else {
            WidgetDiagnostics.favicon.debug(
                "Favicon fetch rejected for \(WidgetDiagnostics.sanitizedURL(url), privacy: .public): empty payload"
            )
            return nil
        }

        guard data.count <= maxFetchedIconBytes else {
            WidgetDiagnostics.favicon.debug(
                "Favicon fetch rejected for \(WidgetDiagnostics.sanitizedURL(url), privacy: .public): payload too large (\(data.count, privacy: .public) bytes)"
            )
            return nil
        }

        return data
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
