import PDFKit
import SwiftUI
#if !targetEnvironment(macCatalyst)
import Textual
#endif

struct ReadabilityReaderView: View {
    @EnvironmentObject private var appModel: AppModel

    let hyperlink: Hyperlink

    @State private var isLoading = false
    @State private var markdown = ""
    @State private var errorMessage: String?
    @State private var contentSourceLabel = ""
    @State private var rendererMode: ReadabilityMarkdownRendererMode = .plainText
    @State private var routedHyperlink: Hyperlink?

    var body: some View {
        Group {
            if isLoading && markdown.isEmpty {
                ProgressView("Loading readability…")
            } else if !markdown.isEmpty {
                ScrollView {
                    HStack {
                        Spacer(minLength: 0)

                        VStack(alignment: .leading, spacing: 16) {
                            if !contentSourceLabel.isEmpty {
                                Text(contentSourceLabel)
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
    }

    private func load() async {
        isLoading = true
        defer { isLoading = false }

        if let client = appModel.apiClient,
           let remoteMarkdown = try? await client.fetchArtifactText(hyperlinkID: hyperlink.id, kind: "readable_text") {
            applyLoadedMarkdown(remoteMarkdown, sourceLabel: "Live version")
            errorMessage = nil
            return
        }

        do {
            let snapshot = try HyperlinkOfflineStore.openShared().snapshot(for: hyperlink.id)
            guard let url = snapshot.readabilityFileURL else {
                markdown = ""
                contentSourceLabel = ""
                rendererMode = .plainText
                errorMessage = snapshot.readabilityError ?? "No offline readability snapshot was saved for this link."
                return
            }
            let loadedMarkdown = try String(contentsOf: url, encoding: .utf8)
            applyLoadedMarkdown(loadedMarkdown, sourceLabel: "Offline snapshot")
            errorMessage = nil
        } catch {
            markdown = ""
            contentSourceLabel = ""
            rendererMode = .plainText
            errorMessage = error.localizedDescription
        }
    }

    private func applyLoadedMarkdown(_ loadedMarkdown: String, sourceLabel: String) {
        markdown = loadedMarkdown
        contentSourceLabel = sourceLabel
        rendererMode = ReadabilityMarkdownRendererMode.preferred(for: loadedMarkdown)
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
        let content = StructuredText(markdown: markdown, baseURL: articleBaseURL)
            .font(.body)
            .foregroundStyle(.primary)
            .textual.structuredTextStyle(.gitHub)
            .textual.overflowMode(.scroll)
            .textual.codeBlockStyle(ReadabilityCodeBlockStyle())
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
        guard let matchedHyperlink = resolveStoredHyperlink(for: url) else {
            return .systemAction(url)
        }

        guard matchedHyperlink.id != hyperlink.id else {
            return .handled
        }

        onOpenHyperlink(matchedHyperlink)
        return .handled
    }

    private func resolveStoredHyperlink(for url: URL) -> Hyperlink? {
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
