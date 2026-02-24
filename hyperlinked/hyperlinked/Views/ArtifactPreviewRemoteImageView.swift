import SwiftUI

struct ArtifactPreviewRemoteImageView: View {
    let url: URL
    let fallbackURL: URL?
    let configuration: ArtifactPreviewImageConfiguration
    let showsFallbackOnFailure: Bool

    var body: some View {
        AsyncImage(url: url) { phase in
            switch phase {
            case .empty:
                ArtifactPreviewPlaceholderView(showProgress: true, configuration: configuration)
            case .success(let image):
                ArtifactPreviewLoadedImageView(image: image, configuration: configuration)
            case .failure:
                failureContent
            @unknown default:
                failureContent
            }
        }
    }

    private var failureContent: some View {
        Group {
            if showsFallbackOnFailure {
                ArtifactPreviewFallbackImageView(
                    fallbackURL: fallbackURL,
                    configuration: configuration
                )
            } else {
                ArtifactPreviewPlaceholderView(showProgress: false, configuration: configuration)
            }
        }
    }
}
