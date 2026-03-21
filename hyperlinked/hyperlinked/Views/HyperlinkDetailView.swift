import SwiftUI

struct HyperlinkDetailView: View {
    @EnvironmentObject private var appModel: AppModel
    @Environment(\.colorScheme) private var colorScheme

    let hyperlinkID: Int
    let fallback: Hyperlink

    @State private var hyperlink: Hyperlink?
    @State private var offlineSnapshot: HyperlinkOfflineSnapshot
    @State private var isLoading = false
    @State private var errorMessage: String?

    init(hyperlinkID: Int, fallback: Hyperlink) {
        self.hyperlinkID = hyperlinkID
        self.fallback = fallback
        _offlineSnapshot = State(initialValue: .empty(hyperlinkID: hyperlinkID))
    }

    var body: some View {
        List {
            if isLoading && hyperlink == nil {
                ProgressView("Loading details…")
            } else {
                HyperlinkDetailSectionsView(
                    hyperlink: hyperlink ?? fallback,
                    colorScheme: colorScheme,
                    offlineSnapshot: offlineSnapshot,
                    onRetryOfflineSave: { retryOfflineSave() }
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

    private func loadOfflineSnapshot() {
        do {
            offlineSnapshot = try HyperlinkOfflineStore.openShared().snapshot(for: hyperlinkID)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func load() async {
        loadOfflineSnapshot()
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

        loadOfflineSnapshot()
    }

    private func retryOfflineSave() {
        guard let client = appModel.apiClient else {
            errorMessage = "No server selected."
            return
        }

        let current = hyperlink ?? fallback
        Task {
            await HyperlinkOfflineSnapshotManager.shared.saveSnapshot(
                for: current,
                client: client,
                includePDF: current.looksLikePDF,
                localPDFSourceURL: nil
            )
            await MainActor.run {
                loadOfflineSnapshot()
            }
        }
    }
}
