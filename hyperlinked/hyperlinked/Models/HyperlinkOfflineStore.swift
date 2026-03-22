import Foundation
import GRDB

enum HyperlinkOfflineAssetState: String, Codable, CaseIterable {
    case missing
    case pending
    case available
    case failed

    var label: String {
        switch self {
        case .missing: return "Not saved"
        case .pending: return "Saving…"
        case .available: return "Saved"
        case .failed: return "Failed"
        }
    }
}

struct HyperlinkOfflineSnapshot: FetchableRecord, PersistableRecord, TableRecord, Equatable {
    static let databaseTableName = DB.hyperlinkOfflineSnapshotTableName

    enum Columns: String, ColumnExpression {
        case hyperlinkID = "hyperlink_id"
        case readabilityState = "readability_state"
        case readabilityPath = "readability_path"
        case readabilityError = "readability_error"
        case readabilitySavedAt = "readability_saved_at"
        case pdfState = "pdf_state"
        case pdfPath = "pdf_path"
        case pdfError = "pdf_error"
        case pdfSavedAt = "pdf_saved_at"
    }

    let hyperlinkID: Int
    let readabilityState: String
    let readabilityPath: String?
    let readabilityError: String?
    let readabilitySavedAt: String?
    let pdfState: String
    let pdfPath: String?
    let pdfError: String?
    let pdfSavedAt: String?

    init(
        hyperlinkID: Int,
        readabilityState: HyperlinkOfflineAssetState = .missing,
        readabilityPath: String? = nil,
        readabilityError: String? = nil,
        readabilitySavedAt: String? = nil,
        pdfState: HyperlinkOfflineAssetState = .missing,
        pdfPath: String? = nil,
        pdfError: String? = nil,
        pdfSavedAt: String? = nil
    ) {
        self.hyperlinkID = hyperlinkID
        self.readabilityState = readabilityState.rawValue
        self.readabilityPath = readabilityPath
        self.readabilityError = readabilityError
        self.readabilitySavedAt = readabilitySavedAt
        self.pdfState = pdfState.rawValue
        self.pdfPath = pdfPath
        self.pdfError = pdfError
        self.pdfSavedAt = pdfSavedAt
    }

    init(row: Row) {
        hyperlinkID = row[Columns.hyperlinkID]
        readabilityState = row[Columns.readabilityState]
        readabilityPath = row[Columns.readabilityPath]
        readabilityError = row[Columns.readabilityError]
        readabilitySavedAt = row[Columns.readabilitySavedAt]
        pdfState = row[Columns.pdfState]
        pdfPath = row[Columns.pdfPath]
        pdfError = row[Columns.pdfError]
        pdfSavedAt = row[Columns.pdfSavedAt]
    }

    func encode(to container: inout PersistenceContainer) {
        container[Columns.hyperlinkID] = hyperlinkID
        container[Columns.readabilityState] = readabilityState
        container[Columns.readabilityPath] = readabilityPath
        container[Columns.readabilityError] = readabilityError
        container[Columns.readabilitySavedAt] = readabilitySavedAt
        container[Columns.pdfState] = pdfState
        container[Columns.pdfPath] = pdfPath
        container[Columns.pdfError] = pdfError
        container[Columns.pdfSavedAt] = pdfSavedAt
    }

    var resolvedReadabilityState: HyperlinkOfflineAssetState {
        HyperlinkOfflineAssetState(rawValue: readabilityState) ?? .missing
    }

    var resolvedPDFState: HyperlinkOfflineAssetState {
        HyperlinkOfflineAssetState(rawValue: pdfState) ?? .missing
    }

    var readabilityFileURL: URL? {
        readabilityPath.map { URL(fileURLWithPath: $0) }
    }

    var pdfFileURL: URL? {
        pdfPath.map { URL(fileURLWithPath: $0) }
    }

    static func empty(hyperlinkID: Int) -> HyperlinkOfflineSnapshot {
        HyperlinkOfflineSnapshot(hyperlinkID: hyperlinkID)
    }
}

final class HyperlinkOfflineStore {
    private let dbQueue: DatabaseQueue
    private let fileManager: FileManager

    init(dbQueue: DatabaseQueue, fileManager: FileManager = .default) {
        self.dbQueue = dbQueue
        self.fileManager = fileManager
    }

    static func openShared() throws -> HyperlinkOfflineStore {
        HyperlinkOfflineStore(dbQueue: try DB.databaseQueue())
    }

    func snapshot(for hyperlinkID: Int) throws -> HyperlinkOfflineSnapshot {
        try dbQueue.read { db in
            try HyperlinkOfflineSnapshot.fetchOne(db, key: hyperlinkID) ?? .empty(hyperlinkID: hyperlinkID)
        }
    }

    func upsert(_ snapshot: HyperlinkOfflineSnapshot) throws {
        try dbQueue.write { db in
            try snapshot.upsert(db)
        }
    }

    func markReadabilityPending(hyperlinkID: Int) throws {
        var snapshot = try snapshot(for: hyperlinkID)
        snapshot = HyperlinkOfflineSnapshot(
            hyperlinkID: hyperlinkID,
            readabilityState: .pending,
            readabilityPath: snapshot.readabilityPath,
            readabilityError: nil,
            readabilitySavedAt: snapshot.readabilitySavedAt,
            pdfState: snapshot.resolvedPDFState,
            pdfPath: snapshot.pdfPath,
            pdfError: snapshot.pdfError,
            pdfSavedAt: snapshot.pdfSavedAt
        )
        try upsert(snapshot)
    }

    func markPDFPending(hyperlinkID: Int) throws {
        var snapshot = try snapshot(for: hyperlinkID)
        snapshot = HyperlinkOfflineSnapshot(
            hyperlinkID: hyperlinkID,
            readabilityState: snapshot.resolvedReadabilityState,
            readabilityPath: snapshot.readabilityPath,
            readabilityError: snapshot.readabilityError,
            readabilitySavedAt: snapshot.readabilitySavedAt,
            pdfState: .pending,
            pdfPath: snapshot.pdfPath,
            pdfError: nil,
            pdfSavedAt: snapshot.pdfSavedAt
        )
        try upsert(snapshot)
    }

    func saveReadability(markdown: String, hyperlinkID: Int) throws {
        let url = try fileURL(for: hyperlinkID, filename: "readability.md")
        try markdown.write(to: url, atomically: true, encoding: .utf8)
        let snapshot = try snapshot(for: hyperlinkID)
        try upsert(
            HyperlinkOfflineSnapshot(
                hyperlinkID: hyperlinkID,
                readabilityState: .available,
                readabilityPath: url.path,
                readabilityError: nil,
                readabilitySavedAt: ISO8601DateFormatter().string(from: Date()),
                pdfState: snapshot.resolvedPDFState,
                pdfPath: snapshot.pdfPath,
                pdfError: snapshot.pdfError,
                pdfSavedAt: snapshot.pdfSavedAt
            )
        )
    }

    func savePDF(data: Data, hyperlinkID: Int) throws {
        let url = try fileURL(for: hyperlinkID, filename: "source.pdf")
        try data.write(to: url, options: .atomic)
        let snapshot = try snapshot(for: hyperlinkID)
        try upsert(
            HyperlinkOfflineSnapshot(
                hyperlinkID: hyperlinkID,
                readabilityState: snapshot.resolvedReadabilityState,
                readabilityPath: snapshot.readabilityPath,
                readabilityError: snapshot.readabilityError,
                readabilitySavedAt: snapshot.readabilitySavedAt,
                pdfState: .available,
                pdfPath: url.path,
                pdfError: nil,
                pdfSavedAt: ISO8601DateFormatter().string(from: Date())
            )
        )
    }

    func copyPDF(from sourceURL: URL, hyperlinkID: Int) throws {
        let destinationURL = try fileURL(for: hyperlinkID, filename: "source.pdf")
        if fileManager.fileExists(atPath: destinationURL.path) {
            try fileManager.removeItem(at: destinationURL)
        }
        try fileManager.copyItem(at: sourceURL, to: destinationURL)
        let snapshot = try snapshot(for: hyperlinkID)
        try upsert(
            HyperlinkOfflineSnapshot(
                hyperlinkID: hyperlinkID,
                readabilityState: snapshot.resolvedReadabilityState,
                readabilityPath: snapshot.readabilityPath,
                readabilityError: snapshot.readabilityError,
                readabilitySavedAt: snapshot.readabilitySavedAt,
                pdfState: .available,
                pdfPath: destinationURL.path,
                pdfError: nil,
                pdfSavedAt: ISO8601DateFormatter().string(from: Date())
            )
        )
    }

    func markReadabilityFailed(hyperlinkID: Int, message: String) throws {
        let snapshot = try snapshot(for: hyperlinkID)
        try upsert(
            HyperlinkOfflineSnapshot(
                hyperlinkID: hyperlinkID,
                readabilityState: .failed,
                readabilityPath: snapshot.readabilityPath,
                readabilityError: message,
                readabilitySavedAt: snapshot.readabilitySavedAt,
                pdfState: snapshot.resolvedPDFState,
                pdfPath: snapshot.pdfPath,
                pdfError: snapshot.pdfError,
                pdfSavedAt: snapshot.pdfSavedAt
            )
        )
    }

    func markPDFFailed(hyperlinkID: Int, message: String) throws {
        let snapshot = try snapshot(for: hyperlinkID)
        try upsert(
            HyperlinkOfflineSnapshot(
                hyperlinkID: hyperlinkID,
                readabilityState: snapshot.resolvedReadabilityState,
                readabilityPath: snapshot.readabilityPath,
                readabilityError: snapshot.readabilityError,
                readabilitySavedAt: snapshot.readabilitySavedAt,
                pdfState: .failed,
                pdfPath: snapshot.pdfPath,
                pdfError: message,
                pdfSavedAt: snapshot.pdfSavedAt
            )
        )
    }

    func deleteSnapshots(for hyperlinkIDs: [Int]) throws {
        guard !hyperlinkIDs.isEmpty else {
            return
        }

        let snapshots = try hyperlinkIDs.map { try snapshot(for: $0) }
        try dbQueue.write { db in
            let placeholders = Array(repeating: "?", count: hyperlinkIDs.count).joined(separator: ",")
            try db.execute(
                sql: "DELETE FROM \(DB.hyperlinkOfflineSnapshotTableName) WHERE hyperlink_id IN (\(placeholders))",
                arguments: StatementArguments(hyperlinkIDs)
            )
        }

        for snapshot in snapshots {
            for path in [snapshot.readabilityPath, snapshot.pdfPath].compactMap({ $0 }) {
                try? fileManager.removeItem(at: URL(fileURLWithPath: path))
            }
            let directory = offlineDirectory(for: snapshot.hyperlinkID)
            try? fileManager.removeItem(at: directory)
        }
    }

    func clearAll() throws {
        let snapshots = try dbQueue.read { db in
            try HyperlinkOfflineSnapshot.fetchAll(db)
        }
        try dbQueue.write { db in
            _ = try HyperlinkOfflineSnapshot.deleteAll(db)
        }
        for snapshot in snapshots {
            for path in [snapshot.readabilityPath, snapshot.pdfPath].compactMap({ $0 }) {
                try? fileManager.removeItem(at: URL(fileURLWithPath: path))
            }
        }
        let directory = rootDirectory()
        if fileManager.fileExists(atPath: directory.path) {
            try? fileManager.removeItem(at: directory)
        }
    }

    private func fileURL(for hyperlinkID: Int, filename: String) throws -> URL {
        let directory = offlineDirectory(for: hyperlinkID)
        try fileManager.createDirectory(at: directory, withIntermediateDirectories: true)
        return directory.appendingPathComponent(filename, isDirectory: false)
    }

    private func offlineDirectory(for hyperlinkID: Int) -> URL {
        rootDirectory().appendingPathComponent(String(hyperlinkID), isDirectory: true)
    }

    private func rootDirectory() -> URL {
        DB.path
            .deletingLastPathComponent()
            .appendingPathComponent("OfflineHyperlinks", isDirectory: true)
    }
}

actor HyperlinkOfflineSnapshotManager {
    static let shared = HyperlinkOfflineSnapshotManager()
    private let logger = AppEventLogger(component: "HyperlinkOfflineSnapshotManager")

    func needsBackfill(for hyperlink: Hyperlink) -> Bool {
        guard let store = try? HyperlinkOfflineStore.openShared(),
              let snapshot = try? store.snapshot(for: hyperlink.id) else {
            return true
        }

        let needsReadability = snapshot.resolvedReadabilityState == .missing
        let needsPDF = hyperlink.looksLikePDF && snapshot.resolvedPDFState == .missing
        return needsReadability || needsPDF
    }

    func backfillMissingSnapshots(hyperlinks: [Hyperlink], client: APIClient) async {
        logger.log("offline_snapshot_backfill_started", details: ["hyperlink_count": String(hyperlinks.count)])
        for hyperlink in hyperlinks {
            guard !Task.isCancelled else {
                logger.log("offline_snapshot_backfill_cancelled")
                return
            }
            guard needsBackfill(for: hyperlink) else {
                continue
            }
            await saveSnapshot(
                for: hyperlink,
                client: client,
                includePDF: hyperlink.looksLikePDF,
                localPDFSourceURL: nil
            )
        }
        logger.log("offline_snapshot_backfill_completed", details: ["hyperlink_count": String(hyperlinks.count)])
    }

    func saveSnapshot(
        for hyperlink: Hyperlink,
        client: APIClient,
        includePDF: Bool,
        localPDFSourceURL: URL? = nil
    ) async {
        guard let store = try? HyperlinkOfflineStore.openShared() else {
            logger.log(
                "offline_snapshot_save_skipped",
                details: ["hyperlink_id": String(hyperlink.id), "reason": "store_open_failed"]
            )
            return
        }

        logger.log(
            "offline_snapshot_save_started",
            details: [
                "hyperlink_id": String(hyperlink.id),
                "include_pdf": includePDF ? "true" : "false",
                "has_local_pdf_source": localPDFSourceURL == nil ? "false" : "true",
            ]
        )

        do {
            try store.markReadabilityPending(hyperlinkID: hyperlink.id)
            let markdown = try await client.fetchArtifactText(hyperlinkID: hyperlink.id, kind: "readable_text")
            try store.saveReadability(markdown: markdown, hyperlinkID: hyperlink.id)
            logger.log("offline_readability_save_succeeded", details: ["hyperlink_id": String(hyperlink.id)])
        } catch {
            try? store.markReadabilityFailed(hyperlinkID: hyperlink.id, message: error.localizedDescription)
            logger.logError("offline_readability_save_failed", error: error, details: ["hyperlink_id": String(hyperlink.id)])
        }

        guard includePDF else {
            logger.log("offline_snapshot_save_completed", details: ["hyperlink_id": String(hyperlink.id), "included_pdf": "false"])
            return
        }

        do {
            try store.markPDFPending(hyperlinkID: hyperlink.id)
            if let localPDFSourceURL {
                try store.copyPDF(from: localPDFSourceURL, hyperlinkID: hyperlink.id)
            } else {
                let data = try await client.fetchArtifactData(hyperlinkID: hyperlink.id, kind: "pdf_source")
                try store.savePDF(data: data, hyperlinkID: hyperlink.id)
            }
            logger.log("offline_pdf_save_succeeded", details: ["hyperlink_id": String(hyperlink.id)])
        } catch {
            try? store.markPDFFailed(hyperlinkID: hyperlink.id, message: error.localizedDescription)
            logger.logError("offline_pdf_save_failed", error: error, details: ["hyperlink_id": String(hyperlink.id)])
        }
        logger.log("offline_snapshot_save_completed", details: ["hyperlink_id": String(hyperlink.id), "included_pdf": "true"])
    }
}

extension Hyperlink {
    var looksLikePDF: Bool {
        [url, rawURL].contains { candidate in
            if let parsed = URL(string: candidate), parsed.pathExtension.lowercased() == "pdf" {
                return true
            }
            return candidate.lowercased().contains(".pdf")
        }
    }
}
