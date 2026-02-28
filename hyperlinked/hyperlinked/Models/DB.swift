import Foundation
import GRDB
import GRDBQuery

enum DBError: LocalizedError {
    case appGroupContainerUnavailable(String)
    case unableToCreateDirectory(URL)

    var errorDescription: String? {
        switch self {
        case .appGroupContainerUnavailable(let appGroupID):
            return "Could not access shared app group storage for \(appGroupID)."
        case .unableToCreateDirectory(let url):
            return "Could not create database directory at \(url.path)."
        }
    }
}

public struct DB {
    nonisolated static let appGroupID = "group.fm.folder.hyperlinked"
    nonisolated static let databaseFilename = "db.sqlite"
    nonisolated static let outboxTableName = "share_outbox_items"
    nonisolated static let hyperlinkTableName = "hyperlink_records"

    nonisolated static var path: URL {
        do {
            return try resolvePath()
        } catch {
            return FileManager.default.temporaryDirectory
                .appendingPathComponent(databaseFilename, isDirectory: false)
        }
    }

    @MainActor static func databaseContext() -> DatabaseContext {
        .readWrite {
            try databaseQueue()
        }
    }

    nonisolated static func databaseQueue() throws -> DatabaseQueue {
        try queueResult.get()
    }

    nonisolated private static let queueResult: Result<DatabaseQueue, Error> = {
        do {
            let dbURL = try resolvePath()
            var configuration = Configuration()
            configuration.busyMode = .timeout(5)
            configuration.prepareDatabase { db in
                try db.execute(sql: "PRAGMA foreign_keys = ON")
                try db.execute(sql: "PRAGMA journal_mode = WAL")
            }

            let queue = try DatabaseQueue(path: dbURL.path, configuration: configuration)
            try migrateIfNeeded(queue: queue)
            return .success(queue)
        } catch {
            return .failure(error)
        }
    }()

    nonisolated private static func resolvePath() throws -> URL {
        guard let containerURL = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: appGroupID
        ) else {
            throw DBError.appGroupContainerUnavailable(appGroupID)
        }

        let appSupportURL = containerURL
            .appendingPathComponent("Library", isDirectory: true)
            .appendingPathComponent("Application Support", isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: appSupportURL, withIntermediateDirectories: true)
        } catch {
            throw DBError.unableToCreateDirectory(appSupportURL)
        }
        return appSupportURL.appendingPathComponent(databaseFilename, isDirectory: false)
    }

    nonisolated private static func migrateIfNeeded(queue: DatabaseQueue) throws {
        var migrator = DatabaseMigrator()

        migrator.registerMigration("create_share_outbox_items") { db in
            try db.create(table: outboxTableName, ifNotExists: true) { t in
                t.column("id", .text).primaryKey()
                t.column("url", .text).notNull()
                t.column("title", .text).notNull().defaults(to: "")
                t.column("created_at", .double).notNull()
                t.column("state", .text).notNull()
                t.column("attempt_count", .integer).notNull().defaults(to: 0)
                t.column("next_attempt_at", .double).notNull()
                t.column("last_attempt_at", .double)
                t.column("last_error", .text)
                t.column("delivered_at", .double)
            }

            try db.create(
                index: "idx_share_outbox_pending_next_attempt",
                on: outboxTableName,
                columns: ["state", "next_attempt_at"],
                ifNotExists: true
            )
            try db.create(
                index: "idx_share_outbox_created_at",
                on: outboxTableName,
                columns: ["created_at"],
                ifNotExists: true
            )
        }

        migrator.registerMigration("rename_legacy_hyperlink_table") { db in
            let oldTableExists = try tableExists("widget_hyperlink", in: db)
            let newTableExists = try tableExists(hyperlinkTableName, in: db)
            if oldTableExists && !newTableExists {
                try db.execute(sql: "ALTER TABLE widget_hyperlink RENAME TO \(hyperlinkTableName)")
            }
        }

        migrator.registerMigration("create_hyperlink_records_v1") { db in
            try db.create(table: hyperlinkTableName, ifNotExists: true) { t in
                t.column("id", .integer).primaryKey()
                t.column("title", .text).notNull()
                t.column("url", .text).notNull()
                t.column("host", .text).notNull()
                t.column("one_liner", .text).notNull()
                t.column("is_url_valid", .boolean).notNull().defaults(to: true)
                t.column("clicks_count", .integer).notNull().defaults(to: 0)
                t.column("last_clicked_at", .text)
                t.column("discovery_depth", .integer).notNull().defaults(to: 0)
                t.column("created_at", .text).notNull()
                t.column("updated_at", .text).notNull()
                t.column("thumbnail_url", .text)
                t.column("thumbnail_dark_url", .text)
                t.column("discovered_via_json", .text).notNull().defaults(to: "[]")
            }

            try db.create(
                index: "idx_hyperlink_records_discovery_depth_created_at_id",
                on: hyperlinkTableName,
                columns: ["discovery_depth", "created_at", "id"],
                ifNotExists: true
            )
            try db.create(
                index: "idx_hyperlink_records_clicks_count_created_at_id",
                on: hyperlinkTableName,
                columns: ["clicks_count", "created_at", "id"],
                ifNotExists: true
            )
            try db.create(
                index: "idx_hyperlink_records_last_clicked_at_created_at_id",
                on: hyperlinkTableName,
                columns: ["last_clicked_at", "created_at", "id"],
                ifNotExists: true
            )
            try db.create(
                index: "idx_hyperlink_records_updated_at_id",
                on: hyperlinkTableName,
                columns: ["updated_at", "id"],
                ifNotExists: true
            )
        }

        migrator.registerMigration("add_hyperlink_records_is_url_valid_v1") { db in
            guard try tableExists(hyperlinkTableName, in: db) else {
                return
            }

            guard try !columnExists("is_url_valid", in: hyperlinkTableName, db: db) else {
                return
            }

            try db.execute(
                sql: """
                    ALTER TABLE \(hyperlinkTableName)
                    ADD COLUMN is_url_valid INTEGER NOT NULL DEFAULT 1
                """
            )
        }

        migrator.registerMigration("add_hyperlink_records_discovered_via_json_v1") { db in
            guard try tableExists(hyperlinkTableName, in: db) else {
                return
            }

            guard try !columnExists("discovered_via_json", in: hyperlinkTableName, db: db) else {
                return
            }

            try db.alter(table: hyperlinkTableName) { t in
                t.add(column: "discovered_via_json", .text).notNull().defaults(to: "[]")
            }
        }

        migrator.registerMigration("reset_hyperlink_records_v2_single_model") { db in
            try db.drop(table: hyperlinkTableName)

            try db.create(table: hyperlinkTableName) { t in
                t.column("id", .integer).primaryKey()
                t.column("title", .text).notNull()
                t.column("url", .text).notNull()
                t.column("raw_url", .text).notNull()
                t.column("og_description", .text)
                t.column("is_url_valid", .boolean)
                t.column("discovery_depth", .integer)
                t.column("clicks_count", .integer).notNull().defaults(to: 0)
                t.column("last_clicked_at", .text)
                t.column("processing_state", .text).notNull().defaults(to: "ready")
                t.column("created_at", .text).notNull()
                t.column("updated_at", .text).notNull()
                t.column("thumbnail_url", .text)
                t.column("thumbnail_dark_url", .text)
                t.column("screenshot_url", .text)
                t.column("screenshot_dark_url", .text)
                t.column("discovered_via_json", .text).notNull().defaults(to: "[]")
            }

            try db.create(
                index: "idx_hyperlink_records_discovery_depth_created_at_id",
                on: hyperlinkTableName,
                columns: ["discovery_depth", "created_at", "id"],
                ifNotExists: true
            )
            try db.create(
                index: "idx_hyperlink_records_clicks_count_created_at_id",
                on: hyperlinkTableName,
                columns: ["clicks_count", "created_at", "id"],
                ifNotExists: true
            )
            try db.create(
                index: "idx_hyperlink_records_last_clicked_at_created_at_id",
                on: hyperlinkTableName,
                columns: ["last_clicked_at", "created_at", "id"],
                ifNotExists: true
            )
            try db.create(
                index: "idx_hyperlink_records_updated_at_id",
                on: hyperlinkTableName,
                columns: ["updated_at", "id"],
                ifNotExists: true
            )
            try db.create(
                index: "idx_hyperlink_records_created_at_id",
                on: hyperlinkTableName,
                columns: ["created_at", "id"],
                ifNotExists: true
            )
        }

        try migrator.migrate(queue)
    }

    nonisolated private static func tableExists(_ name: String, in db: Database) throws -> Bool {
        let count = try Int.fetchOne(
            db,
            sql: """
                SELECT COUNT(*)
                FROM sqlite_master
                WHERE type = 'table' AND name = ?
            """,
            arguments: [name]
        ) ?? 0
        return count > 0
    }

    nonisolated private static func columnExists(
        _ columnName: String,
        in tableName: String,
        db: Database
    ) throws -> Bool {
        let count = try Int.fetchOne(
            db,
            sql: """
                SELECT COUNT(*)
                FROM pragma_table_info('\(tableName)')
                WHERE name = ?
            """,
            arguments: [columnName]
        ) ?? 0
        return count > 0
    }
}
