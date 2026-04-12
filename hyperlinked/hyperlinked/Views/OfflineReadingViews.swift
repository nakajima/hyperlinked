import PDFKit
import SwiftUI
import WebKit
#if !targetEnvironment(macCatalyst)
import Textual
#endif

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

    private var displayedContentSourceLabel: String {
        guard !contentSourceLabel.isEmpty else {
            return ""
        }

        if isWaitingForRenderedHTML {
            return "\(contentSourceLabel) · generating rendered PDF…"
        }

        return contentSourceLabel
    }

    var body: some View {
        Group {
            if isLoading && markdown.isEmpty && html.isEmpty {
                ProgressView("Loading readability…")
            } else if !html.isEmpty {
                VStack(alignment: .leading, spacing: 12) {
                    if !displayedContentSourceLabel.isEmpty {
                        Text(displayedContentSourceLabel)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .padding(.horizontal)
                    }

                    ReadabilityHTMLContentView(
                        html: html,
                        baseURL: htmlBaseURL,
                        hyperlink: hyperlink,
                        onOpenHyperlink: { routedHyperlink = $0 }
                    )
                }
            } else if !markdown.isEmpty {
                ScrollView {
                    HStack {
                        Spacer(minLength: 0)

                        VStack(alignment: .leading, spacing: 16) {
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
                        .frame(maxWidth: 760, alignment: .leading)
                        .padding(.horizontal)
                        .padding(.vertical, 20)

                        Spacer(minLength: 0)
                    }
                }
            } else {
                ContentUnavailableView(
                    "Readability Unavailable",
                    systemImage: "doc.text.magnifyingglass",
                    description: Text(errorMessage ?? "No saved readability snapshot is available yet.")
                )
            }
        }
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
        }
    }

    @MainActor
    private func load() async {
        cancelPendingHTMLUpgrade()
        isLoading = true
        defer { isLoading = false }

        if let client = appModel.apiClient {
            if let remoteHTML = try? await client.fetchArtifactText(hyperlinkID: hyperlink.id, kind: "readable_html") {
                applyLoadedHTML(
                    remoteHTML,
                    baseURL: client.artifactInlineURL(hyperlinkID: hyperlink.id, kind: "readable_html"),
                    sourceLabel: ReadabilityContentSourceLabel.live
                )
                errorMessage = nil
                return
            }

            if let remoteMarkdown = try? await client.fetchArtifactText(hyperlinkID: hyperlink.id, kind: "readable_text") {
                applyLoadedMarkdown(remoteMarkdown, sourceLabel: ReadabilityContentSourceLabel.live)
                errorMessage = nil
                scheduleHTMLUpgrade(using: client)
                return
            }
        }

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

private enum ReadabilityMarkdownRendererMode {
    case textual
    case plainText

    static func preferred(for markdown: String) -> Self {
        #if targetEnvironment(macCatalyst)
        return .plainText
        #else
        guard !markdown.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return .plainText
        }

        return (try? AttributedString(markdown: markdown)) != nil ? .textual : .plainText
        #endif
    }
}

private struct ReadabilityMarkdownContentView: View {
    let markdown: String
    let hyperlink: Hyperlink
    let rendererMode: ReadabilityMarkdownRendererMode
    let onOpenHyperlink: (Hyperlink) -> Void

    private var articleBaseURL: URL? {
        URL(string: hyperlink.url)
    }

    var body: some View {
        Group {
            switch rendererMode {
            case .textual:
                textualContent
            case .plainText:
                plainTextContent
            }
        }
    }

    @ViewBuilder
    private var textualContent: some View {
        #if targetEnvironment(macCatalyst)
        plainTextContent
        #else
        let content = StructuredText(
            markdown: markdown,
            baseURL: articleBaseURL,
            syntaxExtensions: [.math]
        )
            .font(.body)
            .foregroundStyle(.primary)
            .textual.structuredTextStyle(.gitHub)
            .textual.overflowMode(.scroll)
            .textual.codeBlockStyle(ReadabilityCodeBlockStyle())
            .textual.mathProperties(.init(fontName: .latinModern, fontScale: 1.08, textAlignment: .center))
            .textual.textSelection(.enabled)
            .environment(\.openURL, OpenURLAction(handler: handleOpenURL))

        if let articleBaseURL {
            content
                .textual.imageAttachmentLoader(.image(relativeTo: articleBaseURL))
        } else {
            content
        }
        #endif
    }

    private var plainTextContent: some View {
        Text(markdown)
            .font(.body)
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func handleOpenURL(_ url: URL) -> OpenURLAction.Result {
        guard let matchedHyperlink = ReadabilityNavigationResolver.resolveStoredHyperlink(for: url) else {
            return .systemAction(url)
        }

        guard matchedHyperlink.id != hyperlink.id else {
            return .handled
        }

        onOpenHyperlink(matchedHyperlink)
        return .handled
    }
}

private struct ReadabilityHTMLContentView: UIViewRepresentable {
    let html: String
    let baseURL: URL?
    let hyperlink: Hyperlink
    let onOpenHyperlink: (Hyperlink) -> Void

    func makeCoordinator() -> Coordinator {
        Coordinator(hyperlink: hyperlink, onOpenHyperlink: onOpenHyperlink)
    }

    func makeUIView(context: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        let preferences = WKWebpagePreferences()
        preferences.allowsContentJavaScript = false
        configuration.defaultWebpagePreferences = preferences

        let webView = WKWebView(frame: .zero, configuration: configuration)
        webView.navigationDelegate = context.coordinator
        webView.isOpaque = false
        webView.backgroundColor = .systemBackground
        webView.scrollView.backgroundColor = .systemBackground
        return webView
    }

    func updateUIView(_ webView: WKWebView, context: Context) {
        context.coordinator.hyperlink = hyperlink
        context.coordinator.onOpenHyperlink = onOpenHyperlink

        if context.coordinator.lastHTML != html || context.coordinator.lastBaseURL != baseURL {
            context.coordinator.lastHTML = html
            context.coordinator.lastBaseURL = baseURL
            webView.loadHTMLString(
                ReadabilityHTMLDocumentStyler.styledHTML(from: html),
                baseURL: baseURL
            )
        }
    }

    final class Coordinator: NSObject, WKNavigationDelegate {
        var hyperlink: Hyperlink
        var onOpenHyperlink: (Hyperlink) -> Void
        var lastHTML = ""
        var lastBaseURL: URL?

        init(hyperlink: Hyperlink, onOpenHyperlink: @escaping (Hyperlink) -> Void) {
            self.hyperlink = hyperlink
            self.onOpenHyperlink = onOpenHyperlink
        }

        func webView(
            _ webView: WKWebView,
            decidePolicyFor navigationAction: WKNavigationAction,
            decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
        ) {
            guard navigationAction.navigationType == .linkActivated,
                  let url = navigationAction.request.url else {
                decisionHandler(.allow)
                return
            }

            guard let matchedHyperlink = ReadabilityNavigationResolver.resolveStoredHyperlink(for: url) else {
                decisionHandler(.allow)
                return
            }

            guard matchedHyperlink.id != hyperlink.id else {
                decisionHandler(.cancel)
                return
            }

            onOpenHyperlink(matchedHyperlink)
            decisionHandler(.cancel)
        }
    }
}

enum ReadabilityHTMLDocumentStyler {
    private static let themeStyleIdentifier = "hyperlinked-readable-html-theme"

    static func styledHTML(from rawHTML: String) -> String {
        guard !rawHTML.contains(themeStyleIdentifier) else {
            return rawHTML
        }

        let trimmed = rawHTML.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            return rawHTML
        }

        let injection = """
        <meta name=\"viewport\" content=\"width=device-width, initial-scale=1, viewport-fit=cover\">
        <meta name=\"color-scheme\" content=\"light dark\">
        <style id=\"\(themeStyleIdentifier)\">
        :root {
          color-scheme: light dark;
          --reader-bg-light: #ffffff;
          --reader-bg-dark: #111113;
          --reader-fg-light: #111113;
          --reader-fg-dark: #f3f3f5;
          --reader-link-light: #0a66d1;
          --reader-link-dark: #8bb8ff;
          --reader-border-dark: rgba(255, 255, 255, 0.12);
          --reader-border-light: rgba(17, 17, 19, 0.10);
          --reader-surface-dark: rgba(255, 255, 255, 0.06);
          --reader-surface-light: rgba(17, 17, 19, 0.03);
        }

        html {
          background: var(--reader-bg-light) !important;
          -webkit-text-size-adjust: 100%;
        }

        body {
          margin: 0 auto;
          padding: 0 0 32px;
          background: transparent !important;
          color: var(--reader-fg-light) !important;
        }

        a,
        a:visited {
          color: var(--reader-link-light);
        }

        img,
        picture img,
        svg,
        canvas,
        video {
          max-width: 100%;
          height: auto;
          border-radius: 12px;
        }

        figure {
          margin: 1.5em 0;
        }

        table {
          width: 100%;
          border-collapse: collapse;
        }

        th,
        td {
          padding: 0.45rem 0.65rem;
        }

        figure,
        img,
        svg,
        canvas,
        table,
        pre,
        blockquote {
          border-color: var(--reader-border-light);
        }

        @media (prefers-color-scheme: dark) {
          html {
            background: var(--reader-bg-dark) !important;
          }

          body,
          article,
          main,
          section,
          div,
          p,
          span,
          li,
          ul,
          ol,
          dl,
          dt,
          dd,
          h1,
          h2,
          h3,
          h4,
          h5,
          h6,
          figcaption,
          caption,
          small,
          strong,
          em,
          sub,
          sup,
          blockquote,
          pre,
          code,
          table,
          thead,
          tbody,
          tr,
          th,
          td {
            color: var(--reader-fg-dark) !important;
          }

          body,
          article,
          main,
          section,
          div,
          p,
          span,
          li,
          ul,
          ol,
          dl,
          dt,
          dd,
          figcaption,
          caption,
          small,
          strong,
          em,
          sub,
          sup {
            background: transparent !important;
          }

          a,
          a *,
          a:visited,
          a:visited * {
            color: var(--reader-link-dark) !important;
          }

          mjx-container,
          mjx-container *,
          .MathJax,
          .MathJax *,
          .math-inline,
          .math-inline *,
          .math-block,
          .math-block * {
            color: var(--reader-fg-dark) !important;
          }

          figure,
          table,
          thead,
          tbody,
          tr,
          th,
          td,
          pre,
          code,
          blockquote {
            border-color: var(--reader-border-dark) !important;
          }

          figure,
          table,
          pre,
          code,
          blockquote {
            background: var(--reader-surface-dark) !important;
          }

          img,
          picture img,
          svg,
          canvas,
          video {
            filter: brightness(0.58) contrast(0.92) saturate(0.88);
            box-shadow: 0 0 0 1px var(--reader-border-dark);
            background: var(--reader-surface-dark);
          }

          [style*='background'],
          [style*='background-color'] {
            background-color: transparent !important;
          }

          [style*='color:'],
          [style*='color: '] {
            color: inherit !important;
          }
        }
        </style>
        """

        if let headCloseRange = trimmed.range(of: "</head>", options: .caseInsensitive) {
            var themedHTML = trimmed
            themedHTML.insert(contentsOf: "\n\(injection)\n", at: headCloseRange.lowerBound)
            return themedHTML
        }

        if let bodyOpenRange = trimmed.range(of: "<body", options: .caseInsensitive) {
            var themedHTML = trimmed
            themedHTML.insert(contentsOf: "<head>\n\(injection)\n</head>\n", at: bodyOpenRange.lowerBound)
            return themedHTML
        }

        return """
        <!DOCTYPE html>
        <html>
          <head>
            \(injection)
          </head>
          <body>
            \(trimmed)
          </body>
        </html>
        """
    }
}

private enum ReadabilityNavigationResolver {
    static func resolveStoredHyperlink(for url: URL) -> Hyperlink? {
        let normalizedURL = HyperlinkURLMatcher.normalizedString(for: url)
        guard let store = try? HyperlinkStore.openShared(),
              let hyperlinks = try? store.fetchAll() else {
            return nil
        }

        return hyperlinks.first { hyperlink in
            HyperlinkURLMatcher.matches(hyperlink: hyperlink, normalizedURL: normalizedURL)
        }
    }
}

private enum HyperlinkURLMatcher {
    nonisolated static func matches(hyperlink: Hyperlink, normalizedURL: String) -> Bool {
        candidateStrings(for: hyperlink).contains(normalizedURL)
    }

    nonisolated static func normalizedString(for url: URL) -> String {
        normalizedString(for: url.absoluteString) ?? url.absoluteString
    }

    nonisolated private static func candidateStrings(for hyperlink: Hyperlink) -> Set<String> {
        Set([hyperlink.url, hyperlink.rawURL].compactMap(normalizedString(for:)))
    }

    nonisolated private static func normalizedString(for rawValue: String) -> String? {
        let trimmed = rawValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty,
              var components = URLComponents(string: trimmed) else {
            return nil
        }

        components.fragment = nil
        components.scheme = components.scheme?.lowercased()
        components.host = components.host?.lowercased()

        if components.path.isEmpty {
            components.path = "/"
        } else if components.path.count > 1, components.path.hasSuffix("/") {
            components.path.removeLast()
        }

        if let port = components.port,
           (components.scheme == "http" && port == 80) || (components.scheme == "https" && port == 443) {
            components.port = nil
        }

        return components.url?.absoluteString ?? trimmed
    }
}

#if !targetEnvironment(macCatalyst)
private struct ReadabilityCodeBlockStyle: StructuredText.CodeBlockStyle {
    func makeBody(configuration: Configuration) -> some View {
        Overflow {
            configuration.label
                .textual.lineSpacing(.fontScaled(0.22))
                .textual.fontScale(0.84)
                .fixedSize(horizontal: false, vertical: true)
                .monospaced()
                .padding(.vertical, 10)
                .padding(.horizontal, 12)
        }
        .background(Color(uiColor: .secondarySystemGroupedBackground))
        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .stroke(Color(uiColor: .separator).opacity(0.35), lineWidth: 1)
        }
        .textual.blockSpacing(.fontScaled(top: 0.8, bottom: 0.15))
    }
}
#endif

struct PDFReaderView: View {
    let fileURL: URL?

    var body: some View {
        Group {
            if let fileURL {
                PDFKitContainerView(fileURL: fileURL)
                    .ignoresSafeArea(edges: .bottom)
            } else {
                ContentUnavailableView(
                    "PDF Unavailable",
                    systemImage: "doc.richtext",
                    description: Text("No saved PDF is available for this link.")
                )
            }
        }
        .navigationTitle("PDF")
        .navigationBarTitleDisplayMode(.inline)
    }
}

private struct PDFKitContainerView: UIViewRepresentable {
    let fileURL: URL

    func makeUIView(context: Context) -> PDFView {
        let view = PDFView()
        view.autoScales = true
        view.displayMode = .singlePageContinuous
        view.displayDirection = .vertical
        view.backgroundColor = .secondarySystemBackground
        return view
    }

    func updateUIView(_ view: PDFView, context: Context) {
        if view.document?.documentURL != fileURL {
            view.document = PDFDocument(url: fileURL)
        }
    }
}

#Preview("Readability Markdown") {
    ScrollView {
        ReadabilityMarkdownContentView(
            markdown: "# Example\n\nInline math: $x^2 + y^2 = z^2$",
            hyperlink: Hyperlink(
                id: 1,
                title: "Example",
                url: "https://example.com/article",
                rawURL: "https://example.com/article",
                summary: nil,
                ogDescription: nil,
                isURLValid: true,
                discoveryDepth: 0,
                clicksCount: 0,
                lastClickedAt: nil,
                processingState: "ready",
                createdAt: "2026-04-10T00:00:00Z",
                updatedAt: "2026-04-10T00:00:00Z",
                thumbnailURL: nil,
                thumbnailDarkURL: nil,
                screenshotURL: nil,
                screenshotDarkURL: nil,
                discoveredVia: []
            ),
            rendererMode: .textual,
            onOpenHyperlink: { _ in }
        )
        .padding()
    }
}

#Preview("Readability HTML") {
    ReadabilityHTMLContentView(
        html: "<html><body style='font-family: -apple-system; color: white; background: black;'><h1>Readable HTML</h1><p>Rendered by Mathpix.</p></body></html>",
        baseURL: URL(string: "https://example.com")!,
        hyperlink: Hyperlink(
            id: 1,
            title: "Example",
            url: "https://example.com/paper.pdf",
            rawURL: "https://example.com/paper.pdf",
            summary: nil,
            ogDescription: nil,
            isURLValid: true,
            discoveryDepth: 0,
            clicksCount: 0,
            lastClickedAt: nil,
            processingState: "ready",
            createdAt: "2026-04-10T00:00:00Z",
            updatedAt: "2026-04-10T00:00:00Z",
            thumbnailURL: nil,
            thumbnailDarkURL: nil,
            screenshotURL: nil,
            screenshotDarkURL: nil,
            discoveredVia: []
        ),
        onOpenHyperlink: { _ in }
    )
}
