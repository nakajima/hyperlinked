import GRDB
import GRDBQuery

extension ShareOutboxStore {
    static func databaseContext() -> DatabaseContext {
        DB.databaseContext()
    }
}

struct PendingShareOutboxItemsRequest: ValueObservationQueryable {
    static let defaultValue: [ShareOutboxItemRecord] = []

    let limit: Int

    func fetch(_ db: Database) throws -> [ShareOutboxItemRecord] {
        try ShareOutboxItemRecord.fetchAll(
            db,
            sql: """
                SELECT *
                FROM \(ShareOutboxItemRecord.databaseTableName)
                WHERE state != ?
                ORDER BY created_at DESC
                LIMIT ?
            """,
            arguments: [ShareOutboxState.delivered.rawValue, limit]
        )
    }
}
