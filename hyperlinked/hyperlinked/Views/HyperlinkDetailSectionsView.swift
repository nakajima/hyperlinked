import SwiftUI

struct HyperlinkDetailSectionsView: View {
    let hyperlink: Hyperlink
    let colorScheme: ColorScheme
    let offlineSnapshot: HyperlinkOfflineSnapshot
    let apiClient: APIClient?
    let onRetryOfflineSave: () -> Void

    private var showsPDFActions: Bool {
        hyperlink.looksLikePDF || offlineSnapshot.resolvedPDFState != .missing || offlineSnapshot.pdfPath != nil
    }

    var body: some View {
        Section("Offline Reading") {
            NavigationLink {
                ReadabilityReaderView(hyperlink: hyperlink)
            } label: {
                ReadabilityNavigationLinkLabel(
                    hyperlink: hyperlink,
                    offlineSnapshot: offlineSnapshot,
                    apiClient: apiClient
                )
            }

            LabeledContent("Readability", value: offlineSnapshot.resolvedReadabilityState.label)

            if let readabilityError = offlineSnapshot.readabilityError,
               offlineSnapshot.resolvedReadabilityState == .failed {
                Text(readabilityError)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            if showsPDFActions {
                NavigationLink {
                    PDFReaderView(fileURL: offlineSnapshot.pdfFileURL)
                } label: {
                    Label("View PDF", systemImage: "doc.richtext")
                }

                LabeledContent("PDF", value: offlineSnapshot.resolvedPDFState.label)

                if let pdfError = offlineSnapshot.pdfError,
                   offlineSnapshot.resolvedPDFState == .failed {
                    Text(pdfError)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            if offlineSnapshot.resolvedReadabilityState == .failed || offlineSnapshot.resolvedPDFState == .failed {
                Button("Retry Offline Save", action: onRetryOfflineSave)
            }
        }

        Section("Screenshot Preview") {
            HyperlinkScreenshotPreviewView(
                hyperlink: hyperlink,
                colorScheme: colorScheme
            )
        }

        Section("Link") {
            LabeledContent("Title", value: hyperlink.title)
            if let summary = hyperlink.summary?.trimmingCharacters(in: .whitespacesAndNewlines),
               !summary.isEmpty {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Summary")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Text(summary)
                        .font(.body)
                }
            }
            LabeledContent("Canonical URL", value: hyperlink.url)
            LabeledContent("Submitted URL", value: hyperlink.rawURL)
        }

        Section("Status") {
            LabeledContent("URL", value: hyperlink.isURLValid == false ? "Invalid" : "Valid")
            LabeledContent("Clicks", value: "\(hyperlink.clicksCount)")
            LabeledContent("Last Clicked", value: hyperlink.lastClickedAt ?? "Never")
        }

        Section("Timestamps") {
            LabeledContent("Created", value: hyperlink.createdAt)
            LabeledContent("Updated", value: hyperlink.updatedAt)
        }
    }
}

private struct ReadabilityNavigationLinkLabel: View {
    let hyperlink: Hyperlink
    let offlineSnapshot: HyperlinkOfflineSnapshot
    let apiClient: APIClient?

    @State private var previewText: String?

    private var taskID: String {
        [
            String(hyperlink.id),
            apiClient?.baseURL.absoluteString ?? "no-api-client",
            offlineSnapshot.readabilityPath ?? "",
            offlineSnapshot.readabilitySavedAt ?? "",
        ].joined(separator: "|")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Label("View Readability", systemImage: "doc.text")

            if let previewText,
               !previewText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                Text(previewText)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .lineLimit(4)
            }
        }
        .task(id: taskID) {
            await loadPreviewText()
        }
    }

    @MainActor
    private func loadPreviewText() async {
        previewText = await resolvePreviewText()
    }

    private func resolvePreviewText() async -> String? {
        if let apiClient {
            if let html = try? await apiClient.fetchArtifactText(
                hyperlinkID: hyperlink.id,
                kind: "readable_html"
            ),
               let preview = ReadabilityPreviewTextExtractor.previewText(fromHTML: html) {
                return preview
            }

            if let markdown = try? await apiClient.fetchArtifactText(
                hyperlinkID: hyperlink.id,
                kind: "readable_text"
            ),
               let preview = ReadabilityPreviewTextExtractor.previewText(fromMarkdown: markdown) {
                return preview
            }
        }

        if let readabilityFileURL = offlineSnapshot.readabilityFileURL,
           let content = try? String(contentsOf: readabilityFileURL, encoding: .utf8) {
            if readabilityFileURL.pathExtension.lowercased() == "html",
               let preview = ReadabilityPreviewTextExtractor.previewText(fromHTML: content) {
                return preview
            }

            if let preview = ReadabilityPreviewTextExtractor.previewText(fromMarkdown: content) {
                return preview
            }
        }

        if let summary = hyperlink.summary,
           let preview = ReadabilityPreviewTextExtractor.previewText(fromPlainText: summary) {
            return preview
        }

        return nil
    }
}

#if DEBUG
struct HyperlinkDetailSectionsView_Previews: PreviewProvider {
    static var previews: some View {
        NavigationStack {
            List {
                HyperlinkDetailSectionsView(
                    hyperlink: Hyperlink(
                        id: 1,
                        title: "From LLMs to LLM-based Agents for Software Engineering",
                        url: "/uploads/1/2408.02479v2.pdf",
                        rawURL: "/uploads/1/2408.02479v2.pdf",
                        summary: "Survey of LLM and agent-based techniques for software engineering, covering code generation, design, testing, maintenance, and evaluation benchmarks.",
                        ogDescription: nil,
                        isURLValid: true,
                        discoveryDepth: 0,
                        clicksCount: 3,
                        lastClickedAt: "2026-04-10T06:07:45Z",
                        processingState: "ready",
                        createdAt: "2026-04-10T06:07:21Z",
                        updatedAt: "2026-04-10T06:08:12Z",
                        thumbnailURL: nil,
                        thumbnailDarkURL: nil,
                        screenshotURL: nil,
                        screenshotDarkURL: nil
                    ),
                    colorScheme: .light,
                    offlineSnapshot: .empty(hyperlinkID: 1),
                    apiClient: nil,
                    onRetryOfflineSave: {}
                )
            }
            .navigationTitle("Details")
        }
        .previewDisplayName("Hyperlink Detail Sections")
    }
}
#endif
