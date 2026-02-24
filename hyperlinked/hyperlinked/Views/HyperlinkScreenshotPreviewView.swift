import QuickLook
import SwiftUI

struct HyperlinkScreenshotPreviewView: View {
    let hyperlink: Hyperlink
    let colorScheme: ColorScheme

    @State private var quickLookURL: URL?
    @State private var temporaryQuickLookURL: URL?
    @State private var isPreparingQuickLook = false
    @State private var quickLookLoadTask: Task<Void, Never>?

    var body: some View {
        let preview = ArtifactPreviewImage(
            primaryURL: primaryURL,
            fallbackURL: fallbackURL,
            contentMode: .fill,
            placeholderSystemImage: "photo.on.rectangle",
            backgroundColor: Color(.secondarySystemFill),
            imageAlignment: .top
        )
        .frame(maxWidth: .infinity)
        .frame(minHeight: 180)
        .frame(maxHeight: 240)
        .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .strokeBorder(Color.secondary.opacity(0.16), lineWidth: 1)
        )
        .overlay {
            if isPreparingQuickLook {
                ZStack {
                    Color.black.opacity(0.24)
                    ProgressView()
                        .progressViewStyle(.circular)
                        .tint(.white)
                }
            }
        }

        if let resolvedURL = resolvedURL {
            Button {
                openQuickLook(for: resolvedURL)
            } label: {
                preview
            }
            .buttonStyle(.plain)
            .disabled(isPreparingQuickLook)
            .listRowInsets(EdgeInsets(top: 0, leading: 0, bottom: 0, trailing: 0))
            .listRowBackground(Color.clear)
            .quickLookPreview($quickLookURL)
            .onDisappear {
                cleanUpQuickLookState()
            }
        } else {
            preview
                .listRowInsets(EdgeInsets(top: 0, leading: 0, bottom: 0, trailing: 0))
                .listRowBackground(Color.clear)
        }
    }

    private var resolvedURL: URL? {
        primaryURL ?? fallbackURL
    }

    private var primaryURL: URL? {
        switch colorScheme {
        case .dark:
            return hyperlink.screenshotDarkURL.flatMap(URL.init(string:))
        default:
            return hyperlink.screenshotURL.flatMap(URL.init(string:))
        }
    }

    private var fallbackURL: URL? {
        switch colorScheme {
        case .dark:
            return hyperlink.screenshotURL.flatMap(URL.init(string:))
        default:
            return hyperlink.screenshotDarkURL.flatMap(URL.init(string:))
        }
    }

    private func openQuickLook(for url: URL) {
        quickLookLoadTask?.cancel()

        if url.isFileURL {
            quickLookURL = url
            return
        }

        isPreparingQuickLook = true
        quickLookLoadTask = Task {
            do {
                let downloadedURL = try await Self.downloadRemoteFile(from: url)
                guard !Task.isCancelled else {
                    try? FileManager.default.removeItem(at: downloadedURL)
                    return
                }

                await MainActor.run {
                    if let previousTemporaryURL = temporaryQuickLookURL,
                       previousTemporaryURL != downloadedURL {
                        try? FileManager.default.removeItem(at: previousTemporaryURL)
                    }
                    temporaryQuickLookURL = downloadedURL
                    quickLookURL = downloadedURL
                    isPreparingQuickLook = false
                }
            } catch is CancellationError {
                await MainActor.run {
                    isPreparingQuickLook = false
                }
            } catch {
                await MainActor.run {
                    isPreparingQuickLook = false
                }
            }
        }
    }

    private func cleanUpQuickLookState() {
        quickLookLoadTask?.cancel()
        quickLookLoadTask = nil
        isPreparingQuickLook = false

        guard let temporaryQuickLookURL else {
            return
        }

        try? FileManager.default.removeItem(at: temporaryQuickLookURL)
        self.temporaryQuickLookURL = nil
    }

    private static func downloadRemoteFile(from url: URL) async throws -> URL {
        let (downloadedURL, response) = try await URLSession.shared.download(from: url)
        let destination = FileManager.default.temporaryDirectory
            .appendingPathComponent("hyperlinked-preview-\(UUID().uuidString)")
            .appendingPathExtension(fileExtension(for: url, response: response))

        try? FileManager.default.removeItem(at: destination)
        try FileManager.default.moveItem(at: downloadedURL, to: destination)
        return destination
    }

    private static func fileExtension(for url: URL, response: URLResponse) -> String {
        if !url.pathExtension.isEmpty {
            return url.pathExtension
        }

        switch response.mimeType {
        case "image/png":
            return "png"
        case "image/gif":
            return "gif"
        case "image/webp":
            return "webp"
        case "image/heic":
            return "heic"
        default:
            return "jpg"
        }
    }
}
