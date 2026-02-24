import SwiftUI

struct ArtifactPreviewImage: View {
    let primaryURL: URL?
    let fallbackURL: URL?
    let contentMode: ContentMode
    let placeholderSystemImage: String
    let backgroundColor: Color
    let imageAlignment: Alignment

    init(
        primaryURL: URL?,
        fallbackURL: URL?,
        contentMode: ContentMode,
        placeholderSystemImage: String,
        backgroundColor: Color,
        imageAlignment: Alignment = .center
    ) {
        self.primaryURL = primaryURL
        self.fallbackURL = fallbackURL
        self.contentMode = contentMode
        self.placeholderSystemImage = placeholderSystemImage
        self.backgroundColor = backgroundColor
        self.imageAlignment = imageAlignment
    }

    var body: some View {
        ArtifactPreviewResolvedImageView(
            primaryURL: primaryURL,
            fallbackURL: fallbackURL,
            configuration: .init(
                contentMode: contentMode,
                placeholderSystemImage: placeholderSystemImage,
                backgroundColor: backgroundColor,
                imageAlignment: imageAlignment
            )
        )
    }
}

struct ArtifactPreviewImageConfiguration {
    let contentMode: ContentMode
    let placeholderSystemImage: String
    let backgroundColor: Color
    let imageAlignment: Alignment
}
