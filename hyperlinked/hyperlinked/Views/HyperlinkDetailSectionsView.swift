import SwiftUI

struct HyperlinkDetailSectionsView: View {
    let hyperlink: Hyperlink
    let colorScheme: ColorScheme
    let offlineSnapshot: HyperlinkOfflineSnapshot
    let onRetryOfflineSave: () -> Void

    private var showsPDFActions: Bool {
        hyperlink.looksLikePDF || offlineSnapshot.resolvedPDFState != .missing || offlineSnapshot.pdfPath != nil
    }

    var body: some View {
        Section("Offline Reading") {
            NavigationLink {
                ReadabilityReaderView(hyperlink: hyperlink)
            } label: {
                Label("View Readability", systemImage: "doc.text")
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
