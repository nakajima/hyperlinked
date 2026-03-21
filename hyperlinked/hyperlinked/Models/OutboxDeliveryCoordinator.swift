import Foundation

struct OutboxDrainResult {
    var attempted = 0
    var delivered = 0
    var failed = 0
}

final class OutboxDeliveryCoordinator {
    private let store: ShareOutboxStore
    private let client: APIClient

    init(store: ShareOutboxStore, client: APIClient) {
        self.store = store
        self.client = client
    }

    func drainDueItems(limit: Int = 20) async -> OutboxDrainResult {
        var result = OutboxDrainResult()
        let dueItems: [ShareOutboxItemRecord]
        let hyperlinkStore = try? HyperlinkStore.openShared()

        do {
            dueItems = try store.dueItems(limit: limit)
        } catch {
            return result
        }

        for item in dueItems {
            result.attempted += 1
            do {
                let created: Hyperlink
                let localPDFSourceURL: URL?
                switch item.resolvedPayloadKind {
                case .url:
                    created = try await client.createHyperlink(title: item.title, url: item.url)
                    localPDFSourceURL = nil
                case .upload:
                    guard item.resolvedUploadType == .pdf else {
                        throw APIClientError.decodingFailed("unsupported queued upload type")
                    }
                    guard let path = item.uploadFilePath,
                          let filename = item.uploadFilename else {
                        throw APIClientError.decodingFailed("queued upload file metadata is missing")
                    }
                    let fileURL = URL(fileURLWithPath: path)
                    created = try await client.uploadPDF(
                        title: item.title,
                        fileURL: fileURL,
                        filename: filename
                    )
                    localPDFSourceURL = fileURL
                }
                if let hyperlinkStore {
                    try? hyperlinkStore.upsert(hyperlink: created)
                }
                if let localPDFSourceURL,
                   let offlineStore = try? HyperlinkOfflineStore.openShared() {
                    do {
                        try offlineStore.markPDFPending(hyperlinkID: created.id)
                        try offlineStore.copyPDF(from: localPDFSourceURL, hyperlinkID: created.id)
                    } catch {
                        try? offlineStore.markPDFFailed(hyperlinkID: created.id, message: error.localizedDescription)
                    }
                }
                Task {
                    await HyperlinkOfflineSnapshotManager.shared.saveSnapshot(
                        for: created,
                        client: client,
                        includePDF: false
                    )
                }
                try store.markDelivered(id: item.id)
                if item.resolvedPayloadKind == .upload {
                    store.removeUploadFileIfPresent(path: item.uploadFilePath)
                }
                result.delivered += 1
            } catch {
                try? store.markAttemptFailed(id: item.id, errorMessage: error.localizedDescription)
                result.failed += 1
            }
        }

        return result
    }
}
