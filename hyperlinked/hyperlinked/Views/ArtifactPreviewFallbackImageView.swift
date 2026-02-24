import SwiftUI

struct ArtifactPreviewFallbackImageView: View {
    let fallbackURL: URL?
    let configuration: ArtifactPreviewImageConfiguration

    var body: some View {
        if let fallbackURL {
            ArtifactPreviewRemoteImageView(
                url: fallbackURL,
                fallbackURL: nil,
                configuration: configuration,
                showsFallbackOnFailure: false
            )
        } else {
            ArtifactPreviewPlaceholderView(showProgress: false, configuration: configuration)
        }
    }
}
