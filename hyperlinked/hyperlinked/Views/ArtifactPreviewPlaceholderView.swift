import SwiftUI

struct ArtifactPreviewPlaceholderView: View {
    let showProgress: Bool
    let configuration: ArtifactPreviewImageConfiguration

    var body: some View {
        ZStack {
            configuration.backgroundColor
            Image(systemName: configuration.placeholderSystemImage)
                .font(.system(size: 20))
                .foregroundStyle(.secondary)
            if showProgress {
                ProgressView()
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
