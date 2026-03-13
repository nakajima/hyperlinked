import Foundation
import GRDB
import GRDBQuery
import WidgetKit

enum WidgetKind {
    static let hyperlinks = "HyperlinksWidget"
}

protocol WidgetTimelineReloading {
    func reloadHyperlinksWidgetTimeline()
}

struct WidgetTimelineReloader: WidgetTimelineReloading {
    func reloadHyperlinksWidgetTimeline() {
        WidgetCenter.shared.reloadTimelines(ofKind: WidgetKind.hyperlinks)
    }
}

final class HyperlinkStore {
    private let dbQueue: DatabaseQueue
    private let timelineReloader: any WidgetTimelineReloading

    init(
        dbQueue: DatabaseQueue,
        timelineReloader: any WidgetTimelineReloading = WidgetTimelineReloader()
    ) {
        self.dbQueue = dbQueue
        self.timelineReloader = timelineReloader
    }

    static func openShared() throws -> HyperlinkStore {
        HyperlinkStore(dbQueue: try DB.databaseQueue())
    }

    func upsert(hyperlinks: [Hyperlink]) throws {
        guard !hyperlinks.isEmpty else {
            return
        }

        try dbQueue.write { db in
            for hyperlink in hyperlinks {
                try hyperlink.upsert(db)
            }
        }
        timelineReloader.reloadHyperlinksWidgetTimeline()
    }

    func upsert(hyperlink: Hyperlink) throws {
        try upsert(hyperlinks: [hyperlink])
    }

    func replaceAll(hyperlinks: [Hyperlink]) throws {
        let fetchedIDs = Set(hyperlinks.map(\.id))
        var didDeleteExistingRows = false

        try dbQueue.write { db in
            if fetchedIDs.isEmpty {
                didDeleteExistingRows = (try Hyperlink.deleteAll(db)) > 0
                return
            }

            for hyperlink in hyperlinks {
                try hyperlink.upsert(db)
            }

            let persistedIDs = try Int.fetchAll(
                db,
                sql: "SELECT id FROM \(DB.hyperlinkTableName)"
            )
            let idsToDelete = persistedIDs.filter { !fetchedIDs.contains($0) }
            guard !idsToDelete.isEmpty else {
                return
            }

            for id in idsToDelete {
                _ = try Hyperlink.deleteOne(db, key: id)
            }
            didDeleteExistingRows = true
        }

        guard !hyperlinks.isEmpty || didDeleteExistingRows else {
            return
        }
        timelineReloader.reloadHyperlinksWidgetTimeline()
    }

    func apply(updatedBatch: UpdatedHyperlinksBatch) throws {
        guard !updatedBatch.changes.isEmpty else {
            return
        }

        try dbQueue.write { db in
            for change in updatedBatch.changes {
                switch change.changeType {
                case .updated:
                    guard let hyperlink = change.hyperlink else {
                        continue
                    }
                    try hyperlink.upsert(db)
                case .deleted:
                    _ = try Hyperlink.deleteOne(db, key: change.id)
                }
            }
        }
        timelineReloader.reloadHyperlinksWidgetTimeline()
    }

    func clearAll() throws {
        try dbQueue.write { db in
            _ = try Hyperlink.deleteAll(db)
        }
        timelineReloader.reloadHyperlinksWidgetTimeline()
    }
}

struct CachedHyperlinksRequest: ValueObservationQueryable {
    static let defaultValue: [Hyperlink] = []

    let limit: Int
    let rootOnly: Bool

    func fetch(_ db: Database) throws -> [Hyperlink] {
        var request = Hyperlink.all()

        if rootOnly {
            let rootFilter = Hyperlink.Columns.discoveryDepth == 0 || Hyperlink.Columns.discoveryDepth == nil
            request = request.filter(rootFilter)
        }

        request = request
            .order(Hyperlink.Columns.createdAt.desc, Hyperlink.Columns.id.desc)
            .limit(limit)

        return try request.fetchAll(db)
    }
}
