import SwiftUI

struct ReadabilityReaderView: View {
    @EnvironmentObject private var appModel: AppModel

    let hyperlink: Hyperlink

    @State private var isLoading = false
    @State private var markdown = ""
    @State private var html = ""
    @State private var htmlBaseURL: URL?
    @State private var errorMessage: String?
    @State private var contentSourceLabel = ""
    @State private var rendererMode: ReadabilityMarkdownRendererMode = .plainText
    @State private var routedHyperlink: Hyperlink?
    @State private var htmlUpgradeTask: Task<Void, Never>?
    @State private var htmlUpgradeTaskToken: UUID?
    @State private var isWaitingForRenderedHTML = false
    @State private var currentReadabilityProgress: Double?
    @State private var pendingReadabilityProgressUpload: Double?
    @State private var progressUploadTask: Task<Void, Never>?
    @State private var hasLoadedInitialReadabilityProgress = false

    private var displayedContentSourceLabel: String {
        guard !contentSourceLabel.isEmpty else {
            return ""
        }

        if isWaitingForRenderedHTML {
            return "\(contentSourceLabel) · generating rendered PDF…"
        }

        return contentSourceLabel
    }

    private var readabilityProgressContentIdentity: String {
        if !html.isEmpty {
            return [
                "html",
                String(hyperlink.id),
                String(html.count),
                htmlBaseURL?.absoluteString ?? "",
            ].joined(separator: "|")
        }

        if !markdown.isEmpty {
            return [
                "markdown",
                String(hyperlink.id),
                String(markdown.count),
                String(describing: rendererMode),
            ].joined(separator: "|")
        }

        return ["empty", String(hyperlink.id)].joined(separator: "|")
    }

    var body: some View {
        Group {
            if isLoading && markdown.isEmpty && html.isEmpty {
                ProgressView("Loading readability…")
            } else if !html.isEmpty {
                ReadabilityHTMLContentView(
                    html: html,
                    baseURL: htmlBaseURL,
                    hyperlink: hyperlink,
                    initialProgress: currentReadabilityProgress,
                    contentIdentity: readabilityProgressContentIdentity,
                    onProgressChanged: handleReadabilityProgressChanged,
                    onOpenHyperlink: { routedHyperlink = $0 }
                )
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            } else if !markdown.isEmpty {
                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        ReadabilityScrollObservationView(
                            initialProgress: currentReadabilityProgress,
                            contentIdentity: readabilityProgressContentIdentity,
                            onProgressChanged: handleReadabilityProgressChanged
                        )
                        .frame(width: 0, height: 0)

                        if !displayedContentSourceLabel.isEmpty {
                            Text(displayedContentSourceLabel)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }

                        ReadabilityMarkdownContentView(
                            markdown: markdown,
                            hyperlink: hyperlink,
                            rendererMode: rendererMode,
                            onOpenHyperlink: { routedHyperlink = $0 }
                        )
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal)
                    .padding(.vertical, 12)
                }
            } else {
                ContentUnavailableView(
                    "Readability Unavailable",
                    systemImage: "doc.text.magnifyingglass",
                    description: Text(errorMessage ?? "No saved readability snapshot is available yet.")
                )
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Color(uiColor: .systemBackground))
        .navigationTitle("Readability")
        .navigationBarTitleDisplayMode(.inline)
        .navigationDestination(item: $routedHyperlink) { matchedHyperlink in
            ReadabilityReaderView(hyperlink: matchedHyperlink)
        }
        .task(id: appModel.selectedServerURL?.absoluteString) {
            await load()
        }
        .onDisappear {
            cancelPendingHTMLUpgrade()
            flushPendingReadabilityProgressUploadImmediately()
        }
    }

    @MainActor
    private func load() async {
        cancelPendingHTMLUpgrade()
        cancelPendingReadabilityProgressUpload()
        currentReadabilityProgress = nil
        hasLoadedInitialReadabilityProgress = false
        isLoading = true
        defer {
            isLoading = false
            hasLoadedInitialReadabilityProgress = true
        }

        if let client = appModel.apiClient {
            if let remoteHTML = try? await client.fetchArtifactText(hyperlinkID: hyperlink.id, kind: "readable_html") {
                applyLoadedHTML(
                    remoteHTML,
                    baseURL: client.artifactInlineURL(hyperlinkID: hyperlink.id, kind: "readable_html"),
                    sourceLabel: ReadabilityContentSourceLabel.live
                )
                errorMessage = nil
            } else if let remoteMarkdown = try? await client.fetchArtifactText(hyperlinkID: hyperlink.id, kind: "readable_text") {
                applyLoadedMarkdown(remoteMarkdown, sourceLabel: ReadabilityContentSourceLabel.live)
                errorMessage = nil
                scheduleHTMLUpgrade(using: client)
            } else {
                loadOfflineContent()
            }
        } else {
            loadOfflineContent()
        }

        guard let client = appModel.apiClient else {
            return
        }

        guard let loadedProgress = try? await client.fetchReadabilityProgress(hyperlinkID: hyperlink.id) else {
            return
        }

        if currentReadabilityProgress == nil {
            currentReadabilityProgress = loadedProgress.progress
        }
    }

    @MainActor
    private func loadOfflineContent() {
        do {
            let snapshot = try HyperlinkOfflineStore.openShared().snapshot(for: hyperlink.id)
            guard let url = snapshot.readabilityFileURL else {
                clearLoadedContent()
                errorMessage = snapshot.readabilityError ?? "No offline readability snapshot was saved for this link."
                return
            }

            let loadedContent = try String(contentsOf: url, encoding: .utf8)
            if url.pathExtension.lowercased() == "html" {
                applyLoadedHTML(
                    loadedContent,
                    baseURL: url.deletingLastPathComponent(),
                    sourceLabel: ReadabilityContentSourceLabel.offlineSnapshot
                )
            } else {
                applyLoadedMarkdown(loadedContent, sourceLabel: ReadabilityContentSourceLabel.offlineSnapshot)
            }
            errorMessage = nil
        } catch {
            clearLoadedContent()
            errorMessage = error.localizedDescription
        }
    }

    @MainActor
    private func handleReadabilityProgressChanged(_ reportedProgress: Double) {
        let normalizedProgress = min(max(reportedProgress, 0), 1)
        if !hasLoadedInitialReadabilityProgress,
           currentReadabilityProgress == nil,
           normalizedProgress <= 0.0005 {
            return
        }

        currentReadabilityProgress = normalizedProgress
        pendingReadabilityProgressUpload = normalizedProgress

        progressUploadTask?.cancel()
        guard appModel.apiClient != nil else {
            return
        }

        progressUploadTask = Task {
            do {
                try await Task.sleep(nanoseconds: 750_000_000)
            } catch {
                return
            }
            await flushPendingReadabilityProgressUpload()
        }
    }

    @MainActor
    private func cancelPendingReadabilityProgressUpload() {
        progressUploadTask?.cancel()
        progressUploadTask = nil
        pendingReadabilityProgressUpload = nil
    }

    @MainActor
    private func flushPendingReadabilityProgressUploadImmediately() {
        guard pendingReadabilityProgressUpload != nil else {
            cancelPendingReadabilityProgressUpload()
            return
        }

        progressUploadTask?.cancel()
        progressUploadTask = Task {
            await flushPendingReadabilityProgressUpload()
        }
    }

    @MainActor
    private func flushPendingReadabilityProgressUpload() async {
        guard let client = appModel.apiClient,
              let pendingReadabilityProgressUpload else {
            progressUploadTask = nil
            return
        }

        let progressToUpload = pendingReadabilityProgressUpload

        do {
            _ = try await client.setReadabilityProgress(
                hyperlinkID: hyperlink.id,
                progress: progressToUpload
            )
            guard !Task.isCancelled else {
                return
            }

            if self.pendingReadabilityProgressUpload == progressToUpload {
                self.pendingReadabilityProgressUpload = nil
                self.progressUploadTask = nil
            } else {
                self.progressUploadTask = Task {
                    await flushPendingReadabilityProgressUpload()
                }
            }
        } catch {
            guard !Task.isCancelled else {
                return
            }
            progressUploadTask = nil
        }
    }

    @MainActor
    private func cancelPendingHTMLUpgrade() {
        htmlUpgradeTask?.cancel()
        htmlUpgradeTask = nil
        htmlUpgradeTaskToken = nil
        isWaitingForRenderedHTML = false
    }

    @MainActor
    private func scheduleHTMLUpgrade(using client: APIClient) {
        guard ReadabilityHTMLUpgradeRetryPlan.shouldRetry(for: hyperlink) else {
            isWaitingForRenderedHTML = false
            return
        }

        cancelPendingHTMLUpgrade()
        let taskToken = UUID()
        htmlUpgradeTaskToken = taskToken
        isWaitingForRenderedHTML = true

        let hyperlinkID = hyperlink.id
        let htmlBaseURL = client.artifactInlineURL(hyperlinkID: hyperlinkID, kind: "readable_html")
        htmlUpgradeTask = Task {
            for retryDelaySeconds in ReadabilityHTMLUpgradeRetryPlan.retryDelaySeconds {
                do {
                    try await Task.sleep(nanoseconds: retryDelaySeconds * 1_000_000_000)
                } catch {
                    await MainActor.run {
                        guard htmlUpgradeTaskToken == taskToken else { return }
                        htmlUpgradeTask = nil
                        htmlUpgradeTaskToken = nil
                        isWaitingForRenderedHTML = false
                    }
                    return
                }

                guard !Task.isCancelled else {
                    await MainActor.run {
                        guard htmlUpgradeTaskToken == taskToken else { return }
                        htmlUpgradeTask = nil
                        htmlUpgradeTaskToken = nil
                        isWaitingForRenderedHTML = false
                    }
                    return
                }

                guard let upgradedHTML = try? await client.fetchArtifactText(hyperlinkID: hyperlinkID, kind: "readable_html") else {
                    continue
                }

                await MainActor.run {
                    guard htmlUpgradeTaskToken == taskToken else { return }
                    applyLoadedHTML(
                        upgradedHTML,
                        baseURL: htmlBaseURL,
                        sourceLabel: ReadabilityContentSourceLabel.live
                    )
                    errorMessage = nil
                    htmlUpgradeTask = nil
                    htmlUpgradeTaskToken = nil
                    isWaitingForRenderedHTML = false
                }
                return
            }

            await MainActor.run {
                guard htmlUpgradeTaskToken == taskToken else { return }
                htmlUpgradeTask = nil
                htmlUpgradeTaskToken = nil
                isWaitingForRenderedHTML = false
            }
        }
    }

    private func clearLoadedContent() {
        markdown = ""
        html = ""
        htmlBaseURL = nil
        contentSourceLabel = ""
        rendererMode = .plainText
        isWaitingForRenderedHTML = false
    }

    private func applyLoadedMarkdown(_ loadedMarkdown: String, sourceLabel: String) {
        markdown = loadedMarkdown
        html = ""
        htmlBaseURL = nil
        contentSourceLabel = sourceLabel
        rendererMode = ReadabilityMarkdownRendererMode.preferred(for: loadedMarkdown)
    }

    private func applyLoadedHTML(_ loadedHTML: String, baseURL: URL?, sourceLabel: String) {
        html = loadedHTML
        htmlBaseURL = baseURL
        markdown = ""
        contentSourceLabel = sourceLabel
        rendererMode = .plainText
        isWaitingForRenderedHTML = false
    }
}

private enum ReadabilityContentSourceLabel {
    static let live = "Live version"
    static let offlineSnapshot = "Offline snapshot"
}

enum ReadabilityHTMLUpgradeRetryPlan {
    static let retryDelaySeconds: [UInt64] = [2, 4, 8, 16]

    static func shouldRetry(for hyperlink: Hyperlink) -> Bool {
        hyperlink.looksLikePDF
    }
}

enum ReadabilityPreviewTextExtractor {
    private static let maxPreviewLength = 280

    static func previewText(fromHTML html: String) -> String? {
        guard let plainText = plainText(fromHTML: html) else {
            return previewText(fromPlainText: html)
        }
        return previewText(fromPlainText: plainText)
    }

    static func plainText(fromHTML html: String) -> String? {
        guard let data = html.data(using: .utf8) else {
            return normalizedText(fromPlainText: html)
        }

        if let attributed = try? NSAttributedString(
            data: data,
            options: [
                .documentType: NSAttributedString.DocumentType.html,
                .characterEncoding: String.Encoding.utf8.rawValue,
            ],
            documentAttributes: nil
        ) {
            return normalizedText(fromPlainText: attributed.string)
        }

        return normalizedText(fromPlainText: html)
    }

    static func previewText(fromMarkdown markdown: String) -> String? {
        previewText(fromPlainText: markdown)
    }

    static func previewText(fromPlainText text: String) -> String? {
        guard let normalized = normalizedText(fromPlainText: text) else {
            return nil
        }

        guard normalized.count > maxPreviewLength else {
            return normalized
        }

        let endIndex = normalized.index(normalized.startIndex, offsetBy: maxPreviewLength)
        let truncated = normalized[..<endIndex]
        let candidate = truncated.lastIndex(of: " ").map { truncated[..<$0] } ?? truncated[...]
        return candidate.trimmingCharacters(in: .whitespacesAndNewlines) + "…"
    }

    static func normalizedText(fromPlainText text: String) -> String? {
        let normalized = text
            .replacingOccurrences(of: "\u{00a0}", with: " ")
            .split(whereSeparator: \.isWhitespace)
            .joined(separator: " ")
            .trimmingCharacters(in: .whitespacesAndNewlines)

        return normalized.isEmpty ? nil : normalized
    }
}
