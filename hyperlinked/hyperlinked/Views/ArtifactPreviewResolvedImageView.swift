import SwiftUI

struct ArtifactPreviewResolvedImageView: View {
    let primaryURL: URL?
    let fallbackURL: URL?
    let configuration: ArtifactPreviewImageConfiguration

    var body: some View {
        if let primaryURL {
            ArtifactPreviewRemoteImageView(
                url: primaryURL,
                fallbackURL: fallbackURL,
                configuration: configuration,
                showsFallbackOnFailure: true
            )
        } else {
            ArtifactPreviewFallbackImageView(
                fallbackURL: fallbackURL,
                configuration: configuration
            )
        }
    }
}
