import PDFKit
import SwiftUI

struct ReadabilityReaderView: View {
    @EnvironmentObject private var appModel: AppModel

    let hyperlink: Hyperlink

    @State private var isLoading = false
    @State private var markdown = ""
    @State private var errorMessage: String?
    @State private var contentSourceLabel = ""

    var body: some View {
        Group {
            if isLoading && markdown.isEmpty {
                ProgressView("Loading readability…")
            } else if !markdown.isEmpty {
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        if !contentSourceLabel.isEmpty {
                            Text(contentSourceLabel)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }

                        if let attributed = try? AttributedString(markdown: markdown) {
                            Text(attributed)
                                .textSelection(.enabled)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        } else {
                            Text(markdown)
                                .textSelection(.enabled)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding()
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
        .task(id: appModel.selectedServerURL?.absoluteString) {
            await load()
        }
    }

    private func load() async {
        isLoading = true
        defer { isLoading = false }

        if let client = appModel.apiClient,
           let remoteMarkdown = try? await client.fetchArtifactText(hyperlinkID: hyperlink.id, kind: "readable_text") {
            markdown = remoteMarkdown
            contentSourceLabel = "Live version"
            errorMessage = nil
            return
        }

        do {
            let snapshot = try HyperlinkOfflineStore.openShared().snapshot(for: hyperlink.id)
            guard let url = snapshot.readabilityFileURL else {
                errorMessage = snapshot.readabilityError ?? "No offline readability snapshot was saved for this link."
                return
            }
            markdown = try String(contentsOf: url, encoding: .utf8)
            contentSourceLabel = "Offline snapshot"
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}

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
