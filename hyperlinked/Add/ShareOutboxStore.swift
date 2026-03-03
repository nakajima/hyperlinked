import Foundation
import GRDB

enum ShareOutboxState: String, Codable {
    case pending
    case failedTransient = "failed_transient"
    case delivered
}

enum ShareOutboxPayloadKind: String, Codable {
    case url
    case upload
}

enum ShareOutboxUploadType: String, Codable {
    case pdf
}

struct ShareOutboxItemRecord: Codable, FetchableRecord, PersistableRecord, Identifiable {
    static let databaseTableName = "share_outbox_items"

    var id: String
    var url: String
    var title: String
    var payloadKind: String
    var uploadType: String?
    var uploadFilePath: String?
    var uploadFilename: String?
    var createdAt: TimeInterval
    var state: String
    var attemptCount: Int
    var nextAttemptAt: TimeInterval
    var lastAttemptAt: TimeInterval?
    var lastError: String?
    var deliveredAt: TimeInterval?

    enum CodingKeys: String, CodingKey, ColumnExpression {
        case id
        case url
        case title
        case payloadKind = "payload_kind"
        case uploadType = "upload_type"
        case uploadFilePath = "upload_file_path"
        case uploadFilename = "upload_filename"
        case createdAt = "created_at"
        case state
        case attemptCount = "attempt_count"
        case nextAttemptAt = "next_attempt_at"
        case lastAttemptAt = "last_attempt_at"
        case lastError = "last_error"
        case deliveredAt = "delivered_at"
    }

    typealias Columns = CodingKeys

    var resolvedPayloadKind: ShareOutboxPayloadKind {
        ShareOutboxPayloadKind(rawValue: payloadKind) ?? .url
    }

    var resolvedUploadType: ShareOutboxUploadType? {
        guard let uploadType else {
            return nil
        }
        return ShareOutboxUploadType(rawValue: uploadType)
    }
}

enum ShareOutboxStoreError: LocalizedError {
    case appGroupContainerUnavailable

    var errorDescription: String? {
        switch self {
        case .appGroupContainerUnavailable:
            return "Could not access shared app group storage."
        }
    }
}

final class ShareOutboxStore {
    static let appGroupID = "group.fm.folder.hyperlinked"
    static let databaseFilename = "db.sqlite"

    private let dbQueue: DatabaseQueue
    private let appGroupID: String

    private init(dbQueue: DatabaseQueue, appGroupID: String) {
        self.dbQueue = dbQueue
        self.appGroupID = appGroupID
    }

    static func openShared(appGroupID: String = appGroupID) throws -> ShareOutboxStore {
        guard let containerURL = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: appGroupID
        ) else {
            throw ShareOutboxStoreError.appGroupContainerUnavailable
        }

        let appSupportURL = containerURL
            .appendingPathComponent("Library", isDirectory: true)
            .appendingPathComponent("Application Support", isDirectory: true)
        try FileManager.default.createDirectory(
            at: appSupportURL,
            withIntermediateDirectories: true
        )

        let dbURL = appSupportURL.appendingPathComponent(databaseFilename, isDirectory: false)
        var configuration = Configuration()
        configuration.busyMode = .timeout(5)
        configuration.prepareDatabase { db in
            try db.execute(sql: "PRAGMA foreign_keys = ON")
            try db.execute(sql: "PRAGMA journal_mode = WAL")
        }

        let queue = try DatabaseQueue(path: dbURL.path, configuration: configuration)
        let store = ShareOutboxStore(dbQueue: queue, appGroupID: appGroupID)
        try store.migrateIfNeeded()
        return store
    }

    func enqueue(url: String, title: String, now: Date = Date()) throws -> ShareOutboxItemRecord {
        let timestamp = now.timeIntervalSince1970
        let item = ShareOutboxItemRecord(
            id: UUID().uuidString,
            url: url,
            title: title,
            payloadKind: ShareOutboxPayloadKind.url.rawValue,
            uploadType: nil,
            uploadFilePath: nil,
            uploadFilename: nil,
            createdAt: timestamp,
            state: ShareOutboxState.pending.rawValue,
            attemptCount: 0,
            nextAttemptAt: timestamp,
            lastAttemptAt: nil,
            lastError: nil,
            deliveredAt: nil
        )
        try dbQueue.write { db in
            try item.insert(db)
        }
        return item
    }

    func enqueueUpload(
        fileURL: URL,
        filename: String,
        title: String,
        uploadType: ShareOutboxUploadType,
        now: Date = Date()
    ) throws -> ShareOutboxItemRecord {
        let timestamp = now.timeIntervalSince1970
        let sanitizedFilename = Self.sanitizeUploadFilename(
            preferred: filename,
            fallback: fileURL.lastPathComponent
        )
        let copiedFileURL = try Self.copyUploadFileToQueue(
            sourceFileURL: fileURL,
            filename: sanitizedFilename,
            appGroupID: appGroupID
        )
        let item = ShareOutboxItemRecord(
            id: UUID().uuidString,
            url: "",
            title: title,
            payloadKind: ShareOutboxPayloadKind.upload.rawValue,
            uploadType: uploadType.rawValue,
            uploadFilePath: copiedFileURL.path,
            uploadFilename: sanitizedFilename,
            createdAt: timestamp,
            state: ShareOutboxState.pending.rawValue,
            attemptCount: 0,
            nextAttemptAt: timestamp,
            lastAttemptAt: nil,
            lastError: nil,
            deliveredAt: nil
        )
        try dbQueue.write { db in
            try item.insert(db)
        }
        return item
    }

    func dueItems(limit: Int = 20, now: Date = Date()) throws -> [ShareOutboxItemRecord] {
        try dbQueue.read { db in
            try ShareOutboxItemRecord.fetchAll(
                db,
                sql: """
                    SELECT *
                    FROM \(ShareOutboxItemRecord.databaseTableName)
                    WHERE state != ?
                      AND next_attempt_at <= ?
                    ORDER BY next_attempt_at ASC, created_at ASC
                    LIMIT ?
                """,
                arguments: [ShareOutboxState.delivered.rawValue, now.timeIntervalSince1970, limit]
            )
        }
    }

    func markDelivered(id: String, now: Date = Date()) throws {
        let timestamp = now.timeIntervalSince1970
        try dbQueue.write { db in
            try db.execute(
                sql: """
                    UPDATE \(ShareOutboxItemRecord.databaseTableName)
                    SET state = ?,
                        delivered_at = ?,
                        last_attempt_at = ?,
                        last_error = NULL
                    WHERE id = ?
                """,
                arguments: [ShareOutboxState.delivered.rawValue, timestamp, timestamp, id]
            )
        }
    }

    func markAttemptFailed(id: String, errorMessage: String, now: Date = Date()) throws {
        try dbQueue.write { db in
            guard let existing = try ShareOutboxItemRecord.fetchOne(db, key: id) else {
                return
            }

            let nowTimestamp = now.timeIntervalSince1970
            let attempts = existing.attemptCount + 1
            let nextAttemptTimestamp = Self.nextAttemptTimestamp(
                attemptCount: attempts,
                nowTimestamp: nowTimestamp
            )

            try db.execute(
                sql: """
                    UPDATE \(ShareOutboxItemRecord.databaseTableName)
                    SET state = ?,
                        attempt_count = ?,
                        last_attempt_at = ?,
                        next_attempt_at = ?,
                        last_error = ?
                    WHERE id = ?
                """,
                arguments: [
                    ShareOutboxState.failedTransient.rawValue,
                    attempts,
                    nowTimestamp,
                    nextAttemptTimestamp,
                    errorMessage,
                    id,
                ]
            )
        }
    }

    func removeUploadFileIfPresent(path: String?) {
        guard let path, !path.isEmpty else {
            return
        }
        try? FileManager.default.removeItem(atPath: path)
    }

    private func migrateIfNeeded() throws {
        var migrator = DatabaseMigrator()
        migrator.registerMigration("create_share_outbox_items") { db in
            try db.create(table: ShareOutboxItemRecord.databaseTableName, ifNotExists: true) { t in
                t.column(ShareOutboxItemRecord.Columns.id.rawValue, .text).primaryKey()
                t.column(ShareOutboxItemRecord.Columns.url.rawValue, .text).notNull()
                t.column(ShareOutboxItemRecord.Columns.title.rawValue, .text).notNull()
                    .defaults(to: "")
                t.column(ShareOutboxItemRecord.Columns.payloadKind.rawValue, .text)
                    .notNull()
                    .defaults(to: ShareOutboxPayloadKind.url.rawValue)
                t.column(ShareOutboxItemRecord.Columns.uploadType.rawValue, .text)
                t.column(ShareOutboxItemRecord.Columns.uploadFilePath.rawValue, .text)
                t.column(ShareOutboxItemRecord.Columns.uploadFilename.rawValue, .text)
                t.column(ShareOutboxItemRecord.Columns.createdAt.rawValue, .double).notNull()
                t.column(ShareOutboxItemRecord.Columns.state.rawValue, .text).notNull()
                t.column(ShareOutboxItemRecord.Columns.attemptCount.rawValue, .integer).notNull()
                    .defaults(to: 0)
                t.column(ShareOutboxItemRecord.Columns.nextAttemptAt.rawValue, .double).notNull()
                t.column(ShareOutboxItemRecord.Columns.lastAttemptAt.rawValue, .double)
                t.column(ShareOutboxItemRecord.Columns.lastError.rawValue, .text)
                t.column(ShareOutboxItemRecord.Columns.deliveredAt.rawValue, .double)
            }

            try db.create(
                index: "idx_share_outbox_pending_next_attempt",
                on: ShareOutboxItemRecord.databaseTableName,
                columns: [
                    ShareOutboxItemRecord.Columns.state.rawValue,
                    ShareOutboxItemRecord.Columns.nextAttemptAt.rawValue,
                ],
                ifNotExists: true
            )
            try db.create(
                index: "idx_share_outbox_created_at",
                on: ShareOutboxItemRecord.databaseTableName,
                columns: [ShareOutboxItemRecord.Columns.createdAt.rawValue],
                ifNotExists: true
            )
        }

        migrator.registerMigration("add_share_outbox_upload_fields_v1") { db in
            guard try Self.tableExists(ShareOutboxItemRecord.databaseTableName, in: db) else {
                return
            }
            if try !Self.columnExists(
                ShareOutboxItemRecord.Columns.payloadKind.rawValue,
                in: ShareOutboxItemRecord.databaseTableName,
                db: db
            ) {
                try db.execute(
                    sql: """
                        ALTER TABLE \(ShareOutboxItemRecord.databaseTableName)
                        ADD COLUMN \(ShareOutboxItemRecord.Columns.payloadKind.rawValue) TEXT NOT NULL DEFAULT '\(ShareOutboxPayloadKind.url.rawValue)'
                    """
                )
            }
            if try !Self.columnExists(
                ShareOutboxItemRecord.Columns.uploadType.rawValue,
                in: ShareOutboxItemRecord.databaseTableName,
                db: db
            ) {
                try db.execute(
                    sql: """
                        ALTER TABLE \(ShareOutboxItemRecord.databaseTableName)
                        ADD COLUMN \(ShareOutboxItemRecord.Columns.uploadType.rawValue) TEXT
                    """
                )
            }
            if try !Self.columnExists(
                ShareOutboxItemRecord.Columns.uploadFilePath.rawValue,
                in: ShareOutboxItemRecord.databaseTableName,
                db: db
            ) {
                try db.execute(
                    sql: """
                        ALTER TABLE \(ShareOutboxItemRecord.databaseTableName)
                        ADD COLUMN \(ShareOutboxItemRecord.Columns.uploadFilePath.rawValue) TEXT
                    """
                )
            }
            if try !Self.columnExists(
                ShareOutboxItemRecord.Columns.uploadFilename.rawValue,
                in: ShareOutboxItemRecord.databaseTableName,
                db: db
            ) {
                try db.execute(
                    sql: """
                        ALTER TABLE \(ShareOutboxItemRecord.databaseTableName)
                        ADD COLUMN \(ShareOutboxItemRecord.Columns.uploadFilename.rawValue) TEXT
                    """
                )
            }
        }
        try migrator.migrate(dbQueue)
    }

    private static func nextAttemptTimestamp(
        attemptCount: Int,
        nowTimestamp: TimeInterval
    ) -> TimeInterval {
        let exponent = max(0, min(attemptCount - 1, 10))
        let baseDelay = min(pow(2.0, Double(exponent)) * 5.0, 3600.0)
        let jitter = Double.random(in: 0...(baseDelay * 0.2))
        return nowTimestamp + baseDelay + jitter
    }

    private static func copyUploadFileToQueue(
        sourceFileURL: URL,
        filename: String,
        appGroupID: String
    ) throws -> URL {
        let destinationDirectory = try uploadQueueDirectoryURL(appGroupID: appGroupID)
        let destination = destinationDirectory.appendingPathComponent(
            "\(UUID().uuidString)-\(filename)",
            isDirectory: false
        )
        let didStartScopedAccess = sourceFileURL.startAccessingSecurityScopedResource()
        defer {
            if didStartScopedAccess {
                sourceFileURL.stopAccessingSecurityScopedResource()
            }
        }

        if FileManager.default.fileExists(atPath: destination.path) {
            try? FileManager.default.removeItem(at: destination)
        }
        try FileManager.default.copyItem(at: sourceFileURL, to: destination)
        return destination
    }

    private static func uploadQueueDirectoryURL(appGroupID: String) throws -> URL {
        guard let containerURL = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: appGroupID
        ) else {
            throw ShareOutboxStoreError.appGroupContainerUnavailable
        }

        let directory = containerURL
            .appendingPathComponent("Library", isDirectory: true)
            .appendingPathComponent("Application Support", isDirectory: true)
            .appendingPathComponent("share_outbox_uploads", isDirectory: true)
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: true
        )
        return directory
    }

    private static func sanitizeUploadFilename(preferred: String, fallback: String) -> String {
        let raw = preferred.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            ? fallback
            : preferred
        let source = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        let lastComponent = source
            .split(whereSeparator: { $0 == "/" || $0 == "\\" })
            .map(String.init)
            .last ?? source
        var output = lastComponent.filter { character in
            character.isLetter || character.isNumber || character == "." || character == "-" || character == "_"
        }
        output = output.trimmingCharacters(in: CharacterSet(charactersIn: "."))
        if output.isEmpty {
            output = "document.pdf"
        }
        if !output.lowercased().hasSuffix(".pdf") {
            output += ".pdf"
        }
        return output
    }

    private static func tableExists(_ name: String, in db: Database) throws -> Bool {
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

    private static func columnExists(
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
