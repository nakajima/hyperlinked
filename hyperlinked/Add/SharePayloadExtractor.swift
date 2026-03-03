import Foundation
import LinkPresentation
import UniformTypeIdentifiers

enum SharedPayload: Hashable {
    case url(URL)
    case pdf(fileURL: URL, filename: String)
}

struct SharedLinkCandidate: Hashable, Identifiable {
    let payload: SharedPayload
    let sourceLabel: String

    var id: String {
        switch payload {
        case .url(let url):
            return "url:\(url.absoluteString)"
        case .pdf(let fileURL, let filename):
            return "pdf:\(filename):\(fileURL.path)"
        }
    }

    var displayValue: String {
        switch payload {
        case .url(let url):
            let host = url.host ?? url.absoluteString
            return "\(host)\(url.path.isEmpty ? "" : url.path)"
        case .pdf(_, let filename):
            return filename
        }
    }

    var detailValue: String {
        switch payload {
        case .url(let url):
            return url.absoluteString
        case .pdf(let fileURL, _):
            return fileURL.path
        }
    }
}

struct ShareExtractionResult {
    let title: String
    let candidates: [SharedLinkCandidate]
}

enum SharePayloadExtractor {
    private static let linkMetadataTypeIdentifier = "com.apple.linkpresentation.metadata"
    private static let possibleTitleKeys: Set<String> = [
        "title", "Title", "name", "Name", "subject", "Subject", "pageTitle"
    ]
    private static let possibleURLKeys: Set<String> = [
        "url", "URL", "link", "Link", "canonicalURL", "canonicalUrl"
    ]

    static func extract(
        from context: NSExtensionContext?,
        composeText: String?
    ) async -> ShareExtractionResult {
        guard let inputItems = context?.inputItems as? [NSExtensionItem] else {
            return ShareExtractionResult(title: "", candidates: [])
        }

        var extractedTitle = ""
        var fileCandidates: [SharedLinkCandidate] = []
        var directURLCandidates: [SharedLinkCandidate] = []
        var textPayloads: [String] = []

        for item in inputItems {
            if extractedTitle.isEmpty {
                extractedTitle = firstNonEmpty(
                    item.attributedTitle?.string,
                    item.userInfo?["title"] as? String
                ) ?? ""
            }
            if let contentText = item.attributedContentText?.string,
               !contentText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                textPayloads.append(contentText)
            }

            let providers = item.attachments ?? []
            for provider in providers {
                if let propertyList = await loadPropertyList(from: provider) {
                    if extractedTitle.isEmpty {
                        extractedTitle = firstNonEmpty(
                            firstString(in: propertyList, matching: possibleTitleKeys),
                            extractedTitle
                        ) ?? extractedTitle
                    }
                    if let rawURL = firstString(in: propertyList, matching: possibleURLKeys),
                       let parsedURL = normalizeURLString(rawURL) {
                        directURLCandidates.append(
                            SharedLinkCandidate(payload: .url(parsedURL), sourceLabel: "Property List")
                        )
                    }
                }

                if extractedTitle.isEmpty {
                    extractedTitle = firstNonEmpty(provider.suggestedName, extractedTitle) ?? extractedTitle
                }

                if extractedTitle.isEmpty,
                   let metadata = await loadLinkMetadata(from: provider) {
                    extractedTitle = firstNonEmpty(
                        metadata.title,
                        extractedTitle
                    ) ?? extractedTitle
                    if let metadataURL = metadata.originalURL ?? metadata.url,
                       let normalized = normalizeURL(metadataURL) {
                        directURLCandidates.append(
                            SharedLinkCandidate(payload: .url(normalized), sourceLabel: "Metadata")
                        )
                    }
                }

                if provider.hasItemConformingToTypeIdentifier(UTType.url.identifier),
                   let url = await loadURL(from: provider) {
                    directURLCandidates.append(
                        SharedLinkCandidate(payload: .url(url), sourceLabel: "Attachment")
                    )
                    continue
                }

                if provider.hasItemConformingToTypeIdentifier(UTType.pdf.identifier),
                   let fileURL = await loadPDFFileCopy(from: provider) {
                    let filename = provider.suggestedName?.trimmingCharacters(in: .whitespacesAndNewlines)
                    let resolvedFilename = filename?.nilIfEmpty ?? fileURL.lastPathComponent
                    fileCandidates.append(
                        SharedLinkCandidate(
                            payload: .pdf(fileURL: fileURL, filename: resolvedFilename),
                            sourceLabel: "Attachment"
                        )
                    )
                    continue
                }

                if provider.hasItemConformingToTypeIdentifier(UTType.plainText.identifier)
                    || provider.hasItemConformingToTypeIdentifier(UTType.text.identifier),
                   let text = await loadText(from: provider) {
                    textPayloads.append(text)
                }
            }
        }

        if let composeText {
            let trimmed = composeText.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty {
                textPayloads.append(trimmed)
            }
        }

        let dedupedDirectURLs = dedupe(directURLCandidates)
        let dedupedFiles = dedupe(fileCandidates)
        if isLikelyURLString(extractedTitle) {
            extractedTitle = ""
        }
        if extractedTitle.isEmpty {
            extractedTitle = inferTitle(
                from: textPayloads,
                knownURLs: dedupedDirectURLs.compactMap { candidate in
                    if case .url(let url) = candidate.payload {
                        return url
                    }
                    return nil
                }
            ) ?? ""
        }
        if !dedupedFiles.isEmpty || !dedupedDirectURLs.isEmpty {
            return ShareExtractionResult(
                title: extractedTitle,
                candidates: dedupedFiles + dedupedDirectURLs
            )
        }

        var parsedFromText: [SharedLinkCandidate] = []
        for text in textPayloads {
            parsedFromText.append(
                contentsOf: detectLinks(in: text).map { url in
                    SharedLinkCandidate(payload: .url(url), sourceLabel: "Detected from text")
                }
            )
        }

        let dedupedParsedURLs = dedupe(parsedFromText)
        if extractedTitle.isEmpty {
            extractedTitle = inferTitle(
                from: textPayloads,
                knownURLs: dedupedParsedURLs.compactMap { candidate in
                    if case .url(let url) = candidate.payload {
                        return url
                    }
                    return nil
                }
            ) ?? ""
        }
        return ShareExtractionResult(title: extractedTitle, candidates: dedupedParsedURLs)
    }

    private static func detectLinks(in text: String) -> [URL] {
        guard let detector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.link.rawValue)
        else {
            return []
        }

        let nsRange = NSRange(text.startIndex..<text.endIndex, in: text)
        return detector.matches(in: text, options: [], range: nsRange)
            .compactMap { $0.url }
            .compactMap(normalizeURL(_:))
    }

    private static func loadURL(from provider: NSItemProvider) async -> URL? {
        if let item = await loadItem(provider: provider, typeIdentifier: UTType.url.identifier) {
            if let url = item as? URL {
                return normalizeURL(url)
            }
            if let text = item as? String,
               let parsed = URL(string: text) {
                return normalizeURL(parsed)
            }
            if let data = item as? Data,
               let text = String(data: data, encoding: .utf8),
               let parsed = URL(string: text) {
                return normalizeURL(parsed)
            }
        }
        return nil
    }

    private static func loadPDFFileCopy(from provider: NSItemProvider) async -> URL? {
        guard provider.hasItemConformingToTypeIdentifier(UTType.pdf.identifier) else {
            return nil
        }

        return await withCheckedContinuation { continuation in
            provider.loadFileRepresentation(forTypeIdentifier: UTType.pdf.identifier) { fileURL, _ in
                guard let fileURL else {
                    continuation.resume(returning: nil)
                    return
                }
                let temporaryDirectory = FileManager.default.temporaryDirectory
                let destination = temporaryDirectory
                    .appendingPathComponent(UUID().uuidString, isDirectory: false)
                    .appendingPathExtension("pdf")
                do {
                    if FileManager.default.fileExists(atPath: destination.path) {
                        try FileManager.default.removeItem(at: destination)
                    }
                    try FileManager.default.copyItem(at: fileURL, to: destination)
                    continuation.resume(returning: destination)
                } catch {
                    continuation.resume(returning: nil)
                }
            }
        }
    }

    private static func loadLinkMetadata(from provider: NSItemProvider) async -> LPLinkMetadata? {
        guard provider.hasItemConformingToTypeIdentifier(linkMetadataTypeIdentifier) else {
            return nil
        }

        guard let item = await loadItem(
            provider: provider,
            typeIdentifier: linkMetadataTypeIdentifier
        ) else {
            return nil
        }

        if let metadata = item as? LPLinkMetadata {
            return metadata
        }

        if let data = item as? Data,
           let decoded = try? NSKeyedUnarchiver.unarchivedObject(
            ofClass: LPLinkMetadata.self,
            from: data
           ) {
            return decoded
        }

        return nil
    }

    private static func loadText(from provider: NSItemProvider) async -> String? {
        if let item = await loadItem(provider: provider, typeIdentifier: UTType.plainText.identifier) {
            if let text = item as? String {
                return text
            }
            if let attributed = item as? NSAttributedString {
                return attributed.string
            }
            if let url = item as? URL {
                return url.absoluteString
            }
            if let data = item as? Data {
                return String(data: data, encoding: .utf8)
            }
        }
        if let item = await loadItem(provider: provider, typeIdentifier: UTType.text.identifier) {
            if let text = item as? String {
                return text
            }
            if let attributed = item as? NSAttributedString {
                return attributed.string
            }
            if let data = item as? Data {
                return String(data: data, encoding: .utf8)
            }
        }
        return nil
    }

    private static func loadPropertyList(from provider: NSItemProvider) async -> Any? {
        guard provider.hasItemConformingToTypeIdentifier(UTType.propertyList.identifier) else {
            return nil
        }
        return await loadItem(provider: provider, typeIdentifier: UTType.propertyList.identifier)
    }

    private static func loadItem(
        provider: NSItemProvider,
        typeIdentifier: String
    ) async -> NSSecureCoding? {
        await withCheckedContinuation { continuation in
            provider.loadItem(forTypeIdentifier: typeIdentifier, options: nil) { item, _ in
                continuation.resume(returning: item)
            }
        }
    }

    private static func normalizeURL(_ url: URL) -> URL? {
        guard var components = URLComponents(url: url, resolvingAgainstBaseURL: true),
              let scheme = components.scheme?.lowercased(),
              (scheme == "http" || scheme == "https"),
              let host = components.host,
              !host.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return nil
        }

        components.user = nil
        components.password = nil
        guard let normalized = components.url else {
            return nil
        }
        return normalized
    }

    private static func dedupe(_ candidates: [SharedLinkCandidate]) -> [SharedLinkCandidate] {
        var seen: Set<String> = []
        var unique: [SharedLinkCandidate] = []
        unique.reserveCapacity(candidates.count)

        for candidate in candidates {
            let key = candidate.id
            if seen.insert(key).inserted {
                unique.append(candidate)
            }
        }
        return unique
    }

    private static func inferTitle(from payloads: [String], knownURLs: [URL]) -> String? {
        guard !payloads.isEmpty else {
            return nil
        }
        let knownURLStrings = Set(knownURLs.map { $0.absoluteString.lowercased() })

        for payload in payloads {
            for rawLine in payload.replacingOccurrences(of: "\r\n", with: "\n").components(separatedBy: .newlines) {
                let line = collapseWhitespace(in: rawLine)
                guard !line.isEmpty else {
                    continue
                }
                if isLikelyURLString(line) || knownURLStrings.contains(line.lowercased()) {
                    continue
                }
                return String(line.prefix(280))
            }
        }

        return nil
    }

    private static func collapseWhitespace(in value: String) -> String {
        value
            .split(whereSeparator: \.isWhitespace)
            .map(String.init)
            .joined(separator: " ")
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private static func isLikelyURLString(_ value: String) -> Bool {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            return false
        }
        if normalizeURLString(trimmed) != nil {
            return true
        }
        return trimmed.lowercased().hasPrefix("www.")
    }

    private static func normalizeURLString(_ value: String) -> URL? {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            return nil
        }
        let candidate = trimmed.contains("://") ? trimmed : "https://\(trimmed)"
        guard let url = URL(string: candidate) else {
            return nil
        }
        return normalizeURL(url)
    }

    private static func firstString(in value: Any, matching keys: Set<String>) -> String? {
        if let dictionary = value as? [AnyHashable: Any] {
            for (rawKey, rawValue) in dictionary {
                guard let key = rawKey as? String else {
                    continue
                }
                if keys.contains(key), let extracted = normalizedText(from: rawValue) {
                    return extracted
                }
            }
            for rawValue in dictionary.values {
                if let extracted = firstString(in: rawValue, matching: keys) {
                    return extracted
                }
            }
        }

        if let array = value as? [Any] {
            for element in array {
                if let extracted = firstString(in: element, matching: keys) {
                    return extracted
                }
            }
        }

        return nil
    }

    private static func normalizedText(from value: Any) -> String? {
        if let text = value as? String {
            let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
            return trimmed.isEmpty ? nil : trimmed
        }
        if let url = value as? URL {
            return url.absoluteString
        }
        return nil
    }

    private static func firstNonEmpty(_ values: String?...) -> String? {
        for value in values {
            if let value, !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                return value
            }
        }
        return nil
    }
}

private extension String {
    var nilIfEmpty: String? {
        isEmpty ? nil : self
    }
}
