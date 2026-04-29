import OSLog
import SwiftUI
import UIKit
import WidgetKit

struct HyperlinksWidgetEntryView: View {
    @Environment(\.widgetFamily) private var widgetFamily

    let entry: HyperlinksEntry

    var body: some View {
        switch entry.status {
        case .loaded:
            loadedView
        case .noServer:
            messageView(
                title: "No Cached Links",
                subtitle: "Open hyperlinked to sync links for the widget."
            )
        case .empty:
            messageView(
                title: "No matching links",
                subtitle: "Try changing widget options."
            )
        case .error:
            messageView(
                title: "Couldn’t refresh links.",
                subtitle: "We’ll try again soon, I promise."
            )
        }
    }

    @ViewBuilder
    private var loadedView: some View {
        let links = Array(entry.hyperlinks.prefix(rowLimit))

        if links.isEmpty {
            messageView(
                title: "No Links",
                subtitle: "Add a link in the app."
            )
        } else if widgetFamily == .systemSmall {
            let first = links[0]
            Link(destination: WidgetTapURLBuilder.destinationURL(for: first)) {
                VStack(alignment: .leading, spacing: 6) {
                    HStack(spacing: 6) {
                        WidgetFaviconView(hyperlink: first, size: 14)
                        Text(first.host)
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                    Text(first.title)
                        .font(.headline)
                        .multilineTextAlignment(.leading)
                        .lineLimit(3)
                    Spacer(minLength: 0)
                    Text(first.oneLiner)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                    rotationStatusFooter
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            }
            .buttonStyle(.plain)
        } else {
            VStack(alignment: .leading, spacing: 8) {
                Spacer(minLength: 0)

                ForEach(Array(links.enumerated()), id: \.element.id) { index, hyperlink in
                    Link(destination: WidgetTapURLBuilder.destinationURL(for: hyperlink)) {
                        HStack(alignment: .firstTextBaseline, spacing: 8) {
                            WidgetFaviconView(hyperlink: hyperlink, size: 16)
                                .alignmentGuide(.firstTextBaseline) { dimensions in
                                    dimensions[VerticalAlignment.center]
                                }
                            VStack(alignment: .leading, spacing: 2) {
                                Text(hyperlink.title)
                                    .font(.subheadline)
                                    .multilineTextAlignment(.leading)
                                    .lineLimit(1)
                                Text(hyperlink.oneLiner)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                    .lineLimit(1)
                            }
                            Spacer(minLength: 0)
                        }
                    }
                    .buttonStyle(.plain)

                    if index < links.count - 1 {
                        Divider()
                    }
                }

                rotationStatusFooter
                Spacer(minLength: 0)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
        }
    }

    @ViewBuilder
    private var rotationStatusFooter: some View {
        if entry.configuration.rotateSlowly {
            if case .paused = entry.rotationStampStatus {
                Text("Rotation paused")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    private func messageView(title: String, subtitle: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
						Spacer()
            Text(title)
                .font(.headline)
                .multilineTextAlignment(.leading)
                .lineLimit(2)

            Text(subtitle)
                .font(.caption)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.leading)
                .lineLimit(4)
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private var rowLimit: Int {
        switch widgetFamily {
        case .systemSmall:
            return 1
        case .systemMedium:
            return 3
        case .systemLarge:
            return 6
        default:
            return 3
        }
    }
}

struct WidgetFaviconView: View {
    let hyperlink: WidgetHyperlink
    let size: CGFloat
    private static let logger = WidgetDiagnostics.favicon

    var body: some View {
        Group {
            if let faviconURL = hyperlink.faviconURL {
                faviconContent(for: faviconURL)
            } else {
                fallbackDot
            }
        }
        .frame(width: size, height: size)
        .clipShape(Circle())
        .overlay(Circle().stroke(Color.primary.opacity(0.12), lineWidth: 0.5))
    }

    @ViewBuilder
    private func faviconContent(for faviconURL: URL) -> some View {
        if faviconURL.isFileURL {
            if let localImage = Self.loadLocalImage(from: faviconURL) {
                Image(uiImage: localImage)
                    .resizable()
                    .scaledToFill()
            } else {
                fallbackDot
            }
        } else {
            AsyncImage(url: faviconURL) { phase in
                switch phase {
                case .success(let image):
                    image
                        .resizable()
                        .scaledToFill()
                case .failure:
                    fallbackDot
                        .onAppear {
                            Self.logger.debug(
                                "Remote favicon render failed \(Self.sanitizedURLString(faviconURL), privacy: .public)"
                            )
                        }
                case .empty:
                    fallbackDot
                @unknown default:
                    fallbackDot
                }
            }
        }
    }

    private static func loadLocalImage(from fileURL: URL) -> UIImage? {
        guard fileURL.isFileURL else {
            return nil
        }

        let path = fileURL.path
        guard !path.isEmpty else {
            logger.debug("Local favicon path was empty")
            return nil
        }
        guard FileManager.default.fileExists(atPath: path),
              FileManager.default.isReadableFile(atPath: path) else {
            logger.debug(
                "Local favicon file missing or unreadable \(sanitizedURLString(fileURL), privacy: .public)"
            )
            return nil
        }

        if let image = UIImage(contentsOfFile: path) {
            return image
        }

        guard let data = try? Data(contentsOf: fileURL),
              let image = UIImage(data: data) else {
            logger.debug(
                "Failed to decode local favicon image \(sanitizedURLString(fileURL), privacy: .public)"
            )
            return nil
        }
        return image
    }

    private static func sanitizedURLString(_ url: URL) -> String {
        WidgetDiagnostics.sanitizedURL(url)
    }

    private var fallbackDot: some View {
        Circle()
            .fill(hyperlink.fallbackColor?.swiftUIColor ?? Self.hostColor(for: hyperlink.host))
    }

    private static func hostColor(for host: String) -> Color {
        let normalized = host.lowercased()
        let hash = normalized.unicodeScalars.reduce(0) { partial, scalar in
            (partial &* 33 &+ Int(scalar.value)) & 0x7fffffff
        }
        let hue = Double(hash % 360) / 360.0
        return Color(hue: hue, saturation: 0.72, brightness: 0.84)
    }
}

struct HyperlinksWidget: Widget {
    private let kind = "HyperlinksWidget"

    var body: some WidgetConfiguration {
        AppIntentConfiguration(
            kind: kind,
            intent: ConfigurationAppIntent.self,
            provider: HyperlinksProvider()
        ) { entry in
            HyperlinksWidgetEntryView(entry: entry)
                .containerBackground(.fill.tertiary, for: .widget)
        }
        .configurationDisplayName("Hyperlinks")
        .description("Browse links from your server directly on your Home Screen.")
        .supportedFamilies([.systemSmall, .systemMedium, .systemLarge])
    }
}

#Preview("Small - Recent", as: .systemSmall) {
    HyperlinksWidget()
} timeline: {
    HyperlinksEntry.preview(configuration: .previewNewestRoot, dataset: .recent)
}

#Preview("Medium - Sparse Metadata", as: .systemMedium) {
    HyperlinksWidget()
} timeline: {
    HyperlinksEntry.preview(configuration: .previewRandomAllUnclicked, dataset: .sparseDescriptions)
}

#Preview("Large - Mixed", as: .systemLarge) {
    HyperlinksWidget()
} timeline: {
    HyperlinksEntry.preview(configuration: .previewNewestRoot, dataset: .mixed)
}

#Preview("Small - No Server", as: .systemSmall) {
    HyperlinksWidget()
} timeline: {
    HyperlinksEntry.noServer(configuration: .previewNewestRoot)
}

#Preview("Medium - No Matching Links", as: .systemMedium) {
    HyperlinksWidget()
} timeline: {
    HyperlinksEntry.empty(configuration: .previewNewestRoot)
}

#Preview("Large - Refresh Error", as: .systemLarge) {
    HyperlinksWidget()
} timeline: {
    HyperlinksEntry.error(configuration: .previewNewestRoot)
}
