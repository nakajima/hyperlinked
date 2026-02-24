import SwiftUI

struct HyperlinkDetailView: View {
    @EnvironmentObject private var appModel: AppModel
    @Environment(\.colorScheme) private var colorScheme

    let hyperlinkID: Int
    let fallback: Hyperlink

    @State private var hyperlink: Hyperlink?
    @State private var isLoading = false
    @State private var errorMessage: String?

    var body: some View {
        List {
            if isLoading && hyperlink == nil {
                ProgressView("Loading details…")
            } else {
                HyperlinkDetailSectionsView(
                    hyperlink: hyperlink ?? fallback,
                    colorScheme: colorScheme
                )
            }

            if let errorMessage {
                Section("Error") {
                    Text(errorMessage)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .navigationTitle((hyperlink ?? fallback).title)
        .navigationBarTitleDisplayMode(.inline)
        .task(id: appModel.selectedServerURL?.absoluteString) {
            await load()
        }
    }

    private func load() async {
        guard let client = appModel.apiClient else {
            errorMessage = "No server selected."
            return
        }

        isLoading = true
        defer { isLoading = false }

        do {
            hyperlink = try await client.fetchHyperlink(id: hyperlinkID)
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}
