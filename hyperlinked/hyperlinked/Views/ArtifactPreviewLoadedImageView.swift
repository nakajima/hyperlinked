import SwiftUI

struct ArtifactPreviewLoadedImageView: View {
    let image: Image
    let configuration: ArtifactPreviewImageConfiguration

    var body: some View {
        image
            .resizable()
            .aspectRatio(contentMode: configuration.contentMode)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: configuration.imageAlignment)
            .clipped()
    }
}
