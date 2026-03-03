import Foundation
import GRDB
import GRDBQuery

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

struct ShareOutboxItemRecord: Codable, FetchableRecord, PersistableRecord, Identifiable, Sendable {
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

    var createdAtDate: Date {
        Date(timeIntervalSince1970: createdAt)
    }

    var isDelivered: Bool {
        state == ShareOutboxState.delivered.rawValue
    }

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

final class ShareOutboxStore {
    static let databaseFilename = DB.databaseFilename

    private let dbQueue: DatabaseQueue

    private init(dbQueue: DatabaseQueue) {
        self.dbQueue = dbQueue
    }

    static func openShared() throws -> ShareOutboxStore {
        return ShareOutboxStore(dbQueue: try DB.databaseQueue())
    }

    static func databaseContext() -> DatabaseContext {
        return DB.databaseContext()
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
            filename: sanitizedFilename
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

    func pendingItems(limit: Int = 200) throws -> [ShareOutboxItemRecord] {
        try dbQueue.read { db in
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

    func pendingCount() throws -> Int {
        try dbQueue.read { db in
            try Int.fetchOne(
                db,
                sql: """
                    SELECT COUNT(*)
                    FROM \(ShareOutboxItemRecord.databaseTableName)
                    WHERE state != ?
                """,
                arguments: [ShareOutboxState.delivered.rawValue]
            ) ?? 0
        }
    }

    func removeUploadFileIfPresent(path: String?) {
        guard let path, !path.isEmpty else {
            return
        }
        try? FileManager.default.removeItem(atPath: path)
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
        filename: String
    ) throws -> URL {
        let destinationDirectory = try uploadQueueDirectoryURL()
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

    private static func uploadQueueDirectoryURL() throws -> URL {
        guard let containerURL = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: DB.appGroupID
        ) else {
            throw DBError.appGroupContainerUnavailable(DB.appGroupID)
        }
        let directory = containerURL
            .appendingPathComponent("Library", isDirectory: true)
            .appendingPathComponent("Application Support", isDirectory: true)
            .appendingPathComponent("share_outbox_uploads", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
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
