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
                let created = try await client.createHyperlink(title: item.title, url: item.url)
                if let hyperlinkStore {
                    try? hyperlinkStore.upsert(hyperlink: created)
                }
                try store.markDelivered(id: item.id)
                result.delivered += 1
            } catch {
                try? store.markAttemptFailed(id: item.id, errorMessage: error.localizedDescription)
                result.failed += 1
            }
        }

        return result
    }
}
