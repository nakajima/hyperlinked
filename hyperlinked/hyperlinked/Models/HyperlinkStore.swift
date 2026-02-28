import Foundation
import GRDB
import GRDBQuery

final class HyperlinkStore {
    private let dbQueue: DatabaseQueue

    private init(dbQueue: DatabaseQueue) {
        self.dbQueue = dbQueue
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
    }

    func upsert(hyperlink: Hyperlink) throws {
        try upsert(hyperlinks: [hyperlink])
    }

    func apply(updatedBatch: UpdatedHyperlinksBatch) throws {
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
    }

    func clearAll() throws {
        try dbQueue.write { db in
            _ = try Hyperlink.deleteAll(db)
        }
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
