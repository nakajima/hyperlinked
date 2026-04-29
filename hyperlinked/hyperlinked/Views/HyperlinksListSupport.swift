import SwiftUI

enum HyperlinkOrderFilter: String, Identifiable {
    case newest
    case relevance
    case oldest
    case mostClicked = "most-clicked"
    case recentlyClicked = "recently-clicked"
    case recentlyShownInWidget = "recently-shown-in-widget"
    case random

    var id: String { rawValue }

    var label: String {
        switch self {
        case .newest:
            return "Newest"
        case .relevance:
            return "Relevance"
        case .oldest:
            return "Oldest"
        case .mostClicked:
            return "Most Clicked"
        case .recentlyClicked:
            return "Recently Clicked"
        case .recentlyShownInWidget:
            return "Recently Shown in Widget"
        case .random:
            return "Random"
        }
    }
}

struct HyperlinkListRowContent: View {
    let hyperlink: Hyperlink
    let colorScheme: ColorScheme

    private var pdfSummary: String? {
        guard hyperlink.looksLikePDF else {
            return nil
        }

        let trimmed = hyperlink.summary?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return trimmed.isEmpty ? nil : trimmed
    }

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            HyperlinkThumbnailView(hyperlink: hyperlink, colorScheme: colorScheme)

            VStack(alignment: .leading, spacing: 4) {
                Text(hyperlink.title)
                    .font(.headline)
                    .lineLimit(2)
                if let pdfSummary {
                    Text(pdfSummary)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .lineLimit(3)
                } else {
                    Text(hyperlink.url)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }

                if pdfSummary == nil, let description = hyperlink.ogDescription {
                    Text(description)
                        .foregroundStyle(.secondary)
                        .font(.caption)
                        .lineLimit(2)
                }

                HStack(spacing: 12) {
                    if let parent = hyperlink.discoveredVia.first {
                        let discoveredLabel = parent.title.trimmingCharacters(
                            in: .whitespacesAndNewlines
                        ).isEmpty ? parent.url : parent.title
                        Text("Discovered via \(discoveredLabel)")
                            .foregroundStyle(.secondary)
                    }

                    if hyperlink.isURLValid == false {
                        Text("Invalid URL")
                            .foregroundStyle(.orange)
                    }
                }
                .font(.caption2)
                .foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 4)
    }
}

struct PendingOutboxRowContent: View {
    let item: ShareOutboxItemRecord

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text(primaryLine)
                    .font(.headline)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                Text(secondaryLine)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                HStack(spacing: 12) {
                    Text("Pending upload")
                    if item.attemptCount > 0 {
                        Text("Retries \(item.attemptCount)")
                    }
                }
                .font(.caption)
                .foregroundStyle(.tertiary)
                if let lastError = item.lastError,
                   !lastError.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    Text(lastError)
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                        .lineLimit(1)
                }
            }
        }
        .padding(.vertical, 4)
        .opacity(0.78)
    }

    private var primaryLine: String {
        let trimmedTitle = item.title.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmedTitle.isEmpty {
            return trimmedTitle
        }
        if item.resolvedPayloadKind == .upload {
            return item.uploadFilename ?? "Queued upload"
        }
        return item.url
    }

    private var secondaryLine: String {
        if item.resolvedPayloadKind == .upload {
            return item.uploadFilename ?? "PDF upload"
        }
        return item.url
    }
}

#if DEBUG
struct HyperlinksListRowPreviews: PreviewProvider {
    static var previews: some View {
        List {
            HyperlinkListRowContent(hyperlink: sampleHyperlink, colorScheme: .light)
            PendingOutboxRowContent(item: samplePendingOutboxItem)
        }
        .listStyle(.plain)
        .previewDisplayName("Hyperlink List Rows")
    }

    private static let sampleHyperlink = Hyperlink(
        id: 1,
        title: "@LiveModel `in SwiftData",
        url: "https://patschbewebblog.com/posts/2-live-model/",
        rawURL: "https://patschbewebblog.com/posts/2-live-model/",
        ogDescription: "A practical walkthrough of live models in SwiftData.",
        isURLValid: true,
        discoveryDepth: 0,
        clicksCount: 2,
        lastClickedAt: "2026-02-28T01:45:00Z",
        processingState: "ready",
        createdAt: "2026-02-27T12:00:00Z",
        updatedAt: "2026-02-28T01:45:00Z",
        thumbnailURL: nil,
        thumbnailDarkURL: nil,
        screenshotURL: nil,
        screenshotDarkURL: nil
    )

    private static let samplePendingOutboxItem = ShareOutboxItemRecord(
        id: "preview-pending-1",
        url: "https://ryanbrewer.dev/posts/sequent-calculus/",
        title: "Par Part 1: Sequent Calculus",
        payloadKind: ShareOutboxPayloadKind.url.rawValue,
        uploadType: nil,
        uploadFilePath: nil,
        uploadFilename: nil,
        createdAt: 1_740_372_000,
        state: ShareOutboxState.pending.rawValue,
        attemptCount: 1,
        nextAttemptAt: 1_740_372_130,
        lastAttemptAt: 1_740_372_090,
        lastError: "Temporary network timeout",
        deliveredAt: nil
    )
}
#endif
