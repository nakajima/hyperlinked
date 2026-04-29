import PDFKit
import SwiftUI
import WebKit
#if !targetEnvironment(macCatalyst)
import Textual
#endif

enum ReadabilityMarkdownRendererMode {
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

struct ReadabilityMarkdownContentView: View {
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

private enum ReadabilityScrollProgressMath {
    static func normalizedProgress(for scrollView: UIScrollView) -> Double {
        let scrollableHeight = self.scrollableHeight(for: scrollView)
        guard scrollableHeight > 0 else {
            return 0
        }

        let offsetY = scrollView.contentOffset.y + scrollView.adjustedContentInset.top
        let clampedOffsetY = min(max(offsetY, 0), scrollableHeight)
        return Double(clampedOffsetY / scrollableHeight)
    }

    static func targetContentOffsetY(for progress: Double, in scrollView: UIScrollView) -> CGFloat? {
        let normalizedProgress = min(max(progress, 0), 1)
        let scrollableHeight = self.scrollableHeight(for: scrollView)

        if scrollableHeight <= 0 {
            return normalizedProgress <= 0.0005 ? -scrollView.adjustedContentInset.top : nil
        }

        return -scrollView.adjustedContentInset.top + scrollableHeight * normalizedProgress
    }

    private static func scrollableHeight(for scrollView: UIScrollView) -> CGFloat {
        let scrollableHeight = scrollView.contentSize.height
            + scrollView.adjustedContentInset.top
            + scrollView.adjustedContentInset.bottom
            - scrollView.bounds.height
        return max(scrollableHeight, 0)
    }
}

private final class ReadabilityScrollProgressController {
    private weak var scrollView: UIScrollView?
    private var contentOffsetObservation: NSKeyValueObservation?
    private var contentSizeObservation: NSKeyValueObservation?
    private var boundsObservation: NSKeyValueObservation?
    private var restoreProgress: Double?
    private var contentIdentity = ""
    private var hasRestoredCurrentContent = false
    private var lastReportedProgress: Double?
    private var isApplyingRestoredOffset = false
    private var onProgressChanged: ((Double) -> Void)?

    func attach(to scrollView: UIScrollView) {
        guard self.scrollView !== scrollView else {
            attemptRestoreIfNeeded(on: scrollView)
            reportProgress(from: scrollView)
            return
        }

        detach()
        self.scrollView = scrollView
        contentOffsetObservation = scrollView.observe(\.contentOffset, options: [.initial, .new]) { [weak self] scrollView, _ in
            self?.reportProgress(from: scrollView)
        }
        contentSizeObservation = scrollView.observe(\.contentSize, options: [.initial, .new]) { [weak self] scrollView, _ in
            self?.attemptRestoreIfNeeded(on: scrollView)
            self?.reportProgress(from: scrollView)
        }
        boundsObservation = scrollView.observe(\.bounds, options: [.initial, .new]) { [weak self] scrollView, _ in
            self?.attemptRestoreIfNeeded(on: scrollView)
            self?.reportProgress(from: scrollView)
        }
    }

    func update(
        initialProgress: Double?,
        contentIdentity: String,
        onProgressChanged: @escaping (Double) -> Void
    ) {
        self.onProgressChanged = onProgressChanged

        if self.contentIdentity != contentIdentity {
            self.contentIdentity = contentIdentity
            restoreProgress = initialProgress
            hasRestoredCurrentContent = false
            lastReportedProgress = nil
        } else if !hasRestoredCurrentContent {
            restoreProgress = initialProgress
        }

        guard let scrollView else {
            return
        }

        attemptRestoreIfNeeded(on: scrollView)
        reportProgress(from: scrollView)
    }

    func detach() {
        contentOffsetObservation = nil
        contentSizeObservation = nil
        boundsObservation = nil
        scrollView = nil
    }

    deinit {
        detach()
    }

    private func attemptRestoreIfNeeded(on scrollView: UIScrollView) {
        guard !hasRestoredCurrentContent,
              let restoreProgress,
              let targetOffsetY = ReadabilityScrollProgressMath.targetContentOffsetY(
                for: restoreProgress,
                in: scrollView
              ) else {
            return
        }

        if abs(scrollView.contentOffset.y - targetOffsetY) < 1 {
            hasRestoredCurrentContent = true
            reportProgress(from: scrollView)
            return
        }

        isApplyingRestoredOffset = true
        scrollView.setContentOffset(
            CGPoint(x: scrollView.contentOffset.x, y: targetOffsetY),
            animated: false
        )

        DispatchQueue.main.async { [weak self, weak scrollView] in
            guard let self, let scrollView else {
                return
            }
            self.isApplyingRestoredOffset = false
            self.hasRestoredCurrentContent = true
            self.reportProgress(from: scrollView)
        }
    }

    private func reportProgress(from scrollView: UIScrollView) {
        guard !isApplyingRestoredOffset else {
            return
        }

        let progress = ReadabilityScrollProgressMath.normalizedProgress(for: scrollView)
        if let lastReportedProgress,
           abs(lastReportedProgress - progress) < 0.0005 {
            return
        }

        lastReportedProgress = progress
        onProgressChanged?(progress)
    }
}

struct ReadabilityScrollObservationView: UIViewRepresentable {
    let initialProgress: Double?
    let contentIdentity: String
    let onProgressChanged: (Double) -> Void

    func makeUIView(context: Context) -> ObservationView {
        ObservationView()
    }

    func updateUIView(_ uiView: ObservationView, context: Context) {
        uiView.update(
            initialProgress: initialProgress,
            contentIdentity: contentIdentity,
            onProgressChanged: onProgressChanged
        )
    }

    final class ObservationView: UIView {
        private let progressController = ReadabilityScrollProgressController()

        override func didMoveToSuperview() {
            super.didMoveToSuperview()
            attachIfNeeded()
        }

        override func didMoveToWindow() {
            super.didMoveToWindow()
            attachIfNeeded()
        }

        func update(
            initialProgress: Double?,
            contentIdentity: String,
            onProgressChanged: @escaping (Double) -> Void
        ) {
            attachIfNeeded()
            progressController.update(
                initialProgress: initialProgress,
                contentIdentity: contentIdentity,
                onProgressChanged: onProgressChanged
            )
        }

        private func attachIfNeeded() {
            var ancestor = superview
            while let current = ancestor {
                if let scrollView = current as? UIScrollView {
                    progressController.attach(to: scrollView)
                    return
                }
                ancestor = current.superview
            }
        }

        deinit {
            progressController.detach()
        }
    }
}

struct ReadabilityHTMLContentView: UIViewRepresentable {
    let html: String
    let baseURL: URL?
    let hyperlink: Hyperlink
    let initialProgress: Double?
    let contentIdentity: String
    let onProgressChanged: (Double) -> Void
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
        webView.scrollView.contentInset = .zero
        webView.scrollView.scrollIndicatorInsets = .zero
        webView.scrollView.contentInsetAdjustmentBehavior = .never
        context.coordinator.progressController.attach(to: webView.scrollView)
        return webView
    }

    func updateUIView(_ webView: WKWebView, context: Context) {
        context.coordinator.hyperlink = hyperlink
        context.coordinator.onOpenHyperlink = onOpenHyperlink
        context.coordinator.progressController.update(
            initialProgress: initialProgress,
            contentIdentity: contentIdentity,
            onProgressChanged: onProgressChanged
        )

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
        fileprivate let progressController = ReadabilityScrollProgressController()

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

        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
            progressController.attach(to: webView.scrollView)
        }

        deinit {
            progressController.detach()
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

        html,
        body {
          min-height: 100%;
        }

        html {
          height: 100%;
          background: var(--reader-bg-light) !important;
          -webkit-text-size-adjust: 100%;
        }

        body {
          margin: 0 !important;
          padding: 0 !important;
          padding-bottom: max(16px, env(safe-area-inset-bottom)) !important;
          min-height: 100vh;
          min-height: 100dvh;
          box-sizing: border-box;
          background: transparent !important;
          color: var(--reader-fg-light) !important;
          font-family: -apple-system, BlinkMacSystemFont, \"SF Pro Text\", system-ui, sans-serif !important;
          line-height: 1.55;
        }

        * {
          font-family: -apple-system, BlinkMacSystemFont, \"SF Pro Text\", system-ui, sans-serif !important;
        }

        code,
        pre,
        kbd,
        samp {
          font-family: ui-monospace, SFMono-Regular, SF Mono, Menlo, Consolas, monospace !important;
        }

        a,
        a:visited {
          color: var(--reader-link-light);
        }

        @media (min-width: 768px) {
          body {
            max-width: 920px;
            margin: 0 auto !important;
            padding-left: 28px !important;
            padding-right: 28px !important;
          }
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

        mjx-container {
          margin: 0.1em 0;
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

private enum ReadabilityPreviewFixtures {
    private static let sourceURLString = "https://links.fishmt.net/hyperlinks/155/artifacts/readable_html/inline"

    static let readableHTML = """
    <article>
      <h1>Associated Types with Class</h1>
      <p>Haskell's type classes allow ad-hoc overloading, or type-indexing, of functions.</p>
      <p>A natural generalisation is to allow type-indexing of data types as well.</p>
    </article>
    """

    static let hyperlink = Hyperlink(
        id: 155,
        title: "Associated Types with Class",
        url: sourceURLString,
        rawURL: sourceURLString,
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
        screenshotDarkURL: nil
    )

    static let readableTranscript = ReadabilityPreviewTextExtractor.plainText(fromHTML: readableHTML)
        ?? "Associated Types with Class"
}

#Preview("Readability Markdown") {
    ScrollView {
        ReadabilityMarkdownContentView(
            markdown: ReadabilityPreviewFixtures.readableTranscript,
            hyperlink: ReadabilityPreviewFixtures.hyperlink,
            rendererMode: .textual,
            onOpenHyperlink: { _ in }
        )
        .padding()
    }
}

#Preview("Readability HTML") {
    ReadabilityHTMLContentView(
        html: ReadabilityPreviewFixtures.readableHTML,
        baseURL: URL(string: "https://links.fishmt.net/hyperlinks/155/artifacts/readable_html/inline")!,
        hyperlink: ReadabilityPreviewFixtures.hyperlink,
        initialProgress: 0.42,
        contentIdentity: "preview-html",
        onProgressChanged: { _ in },
        onOpenHyperlink: { _ in }
    )
}
