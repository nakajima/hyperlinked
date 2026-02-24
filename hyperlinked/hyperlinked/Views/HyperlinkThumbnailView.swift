import SwiftUI

struct HyperlinkThumbnailView: View {
    let hyperlink: Hyperlink
    let colorScheme: ColorScheme

    var body: some View {
        ArtifactPreviewImage(
            primaryURL: primaryURL,
            fallbackURL: fallbackURL,
            contentMode: .fill,
            placeholderSystemImage: "photo",
            backgroundColor: Color(.secondarySystemFill),
            imageAlignment: .top
        )
        .frame(width: 72, height: 72)
        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(Color.secondary.opacity(0.16), lineWidth: 1)
        )
    }

    private var primaryURL: URL? {
        switch colorScheme {
        case .dark:
            return hyperlink.thumbnailDarkURL.flatMap(URL.init(string:))
        default:
            return hyperlink.thumbnailURL.flatMap(URL.init(string:))
        }
    }

    private var fallbackURL: URL? {
        switch colorScheme {
        case .dark:
            return hyperlink.thumbnailURL.flatMap(URL.init(string:))
        default:
            return hyperlink.thumbnailDarkURL.flatMap(URL.init(string:))
        }
    }
}
