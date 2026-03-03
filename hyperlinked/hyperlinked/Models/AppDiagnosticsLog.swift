import Foundation

struct WidgetRotationDiagnosticsSnapshot {
    enum RotationStatus {
        case ok
        case failed
        case noData

        var label: String {
            switch self {
            case .ok:
                return "OK"
            case .failed:
                return "Failed"
            case .noData:
                return "No data"
            }
        }
    }

    let status: RotationStatus
    let lastFailureAt: Date?
    let lastFailureDBMode: String?
    let lastFailureSQLiteCode: Int?
    let lastFailureSQLiteMessage: String?
    let lastFailureStage: String?
    let lastSuccessAt: Date?
    let recentFailureCount24h: Int
    let logFilePath: String
    let logFileSizeBytes: Int64
    let latestLogEntry: String?

    static let empty = WidgetRotationDiagnosticsSnapshot(
        status: .noData,
        lastFailureAt: nil,
        lastFailureDBMode: nil,
        lastFailureSQLiteCode: nil,
        lastFailureSQLiteMessage: nil,
        lastFailureStage: nil,
        lastSuccessAt: nil,
        recentFailureCount24h: 0,
        logFilePath: "",
        logFileSizeBytes: 0,
        latestLogEntry: nil
    )
}

actor AppDiagnosticsLog {
    static let shared = AppDiagnosticsLog()

    private let appGroupID = "group.fm.folder.hyperlinked"
    private let pendingEventsKey = "diagnostics.pending_events.v1"
    private let ingestedEventIDsKey = "diagnostics.ingested_event_ids.v1"
    private let lastFailureAtKey = "diagnostics.widget_rotation.last_failure_at.v1"
    private let lastFailureDBModeKey = "diagnostics.widget_rotation.last_failure_db_mode.v1"
    private let lastFailureSQLiteCodeKey = "diagnostics.widget_rotation.last_failure_sqlite_code.v1"
    private let lastFailureSQLiteMessageKey = "diagnostics.widget_rotation.last_failure_sqlite_message.v1"
    private let lastFailureStageKey = "diagnostics.widget_rotation.last_failure_stage.v1"
    private let lastSuccessAtKey = "diagnostics.widget_rotation.last_success_at.v1"

    private let maxLogBytes = 2 * 1024 * 1024
    private let maxIngestedEventIDs = 500
    private let recentWindow: TimeInterval = 24 * 60 * 60
    private let formatter: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        formatter.timeZone = TimeZone(secondsFromGMT: 0)
        return formatter
    }()

    func refreshSnapshot() -> WidgetRotationDiagnosticsSnapshot {
        ingestPendingEvents()
        return snapshot()
    }

    func clearLog() -> WidgetRotationDiagnosticsSnapshot {
        if let logURL = try? resolveLogURL() {
            try? FileManager.default.removeItem(at: logURL)
        }
        return snapshot()
    }

    func appendAppEvent(
        name: String,
        details: [String: String]
    ) {
        var payload: [String: Any] = [
            "event_id": UUID().uuidString,
            "timestamp": formatter.string(from: .now),
            "subsystem": "app",
            "event": name,
        ]
        if !details.isEmpty {
            payload["details"] = details
        }
        appendJSONPayloads([payload])
    }

    private func ingestPendingEvents() {
        guard let sharedDefaults = UserDefaults(suiteName: appGroupID) else {
            return
        }

        let pending = sharedDefaults.stringArray(forKey: pendingEventsKey) ?? []
        guard !pending.isEmpty else {
            return
        }

        let storedIDs = sharedDefaults.stringArray(forKey: ingestedEventIDsKey) ?? []
        var knownIDs = Set(storedIDs)
        var orderedKnownIDs = storedIDs
        var linesToAppend: [String] = []

        for line in pending {
            guard !line.isEmpty else {
                continue
            }

            if let eventID = eventIdentifier(in: line), knownIDs.contains(eventID) {
                continue
            }

            if let eventID = eventIdentifier(in: line) {
                knownIDs.insert(eventID)
                orderedKnownIDs.append(eventID)
            }

            linesToAppend.append(line)
        }

        guard !linesToAppend.isEmpty else {
            sharedDefaults.removeObject(forKey: pendingEventsKey)
            return
        }

        guard appendLogLines(linesToAppend) else {
            return
        }

        if orderedKnownIDs.count > maxIngestedEventIDs {
            orderedKnownIDs = Array(orderedKnownIDs.suffix(maxIngestedEventIDs))
        }
        sharedDefaults.set(orderedKnownIDs, forKey: ingestedEventIDsKey)
        sharedDefaults.removeObject(forKey: pendingEventsKey)
    }

    private func appendJSONPayloads(_ payloads: [[String: Any]]) {
        let lines = payloads.compactMap { payload -> String? in
            guard JSONSerialization.isValidJSONObject(payload),
                  let data = try? JSONSerialization.data(withJSONObject: payload, options: []),
                  let line = String(data: data, encoding: .utf8) else {
                return nil
            }
            return line
        }
        _ = appendLogLines(lines)
    }

    @discardableResult
    private func appendLogLines(_ lines: [String]) -> Bool {
        guard !lines.isEmpty else {
            return true
        }

        guard let logURL = try? resolveLogURL() else {
            return false
        }

        let payload = lines.joined(separator: "\n") + "\n"
        guard let data = payload.data(using: .utf8) else {
            return false
        }

        do {
            if FileManager.default.fileExists(atPath: logURL.path) {
                let handle = try FileHandle(forWritingTo: logURL)
                defer {
                    try? handle.close()
                }
                try handle.seekToEnd()
                try handle.write(contentsOf: data)
            } else {
                try data.write(to: logURL, options: .atomic)
            }
            try truncateLogIfNeeded(at: logURL)
            return true
        } catch {
            // Ignore diagnostics write failures; app behavior should remain unaffected.
            return false
        }
    }

    private func truncateLogIfNeeded(at url: URL) throws {
        let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
        let fileSize = (attributes[.size] as? NSNumber)?.int64Value ?? 0
        guard fileSize > Int64(maxLogBytes) else {
            return
        }

        let data = try Data(contentsOf: url)
        let tail = Data(data.suffix(maxLogBytes))
        let trimmed: Data
        if let newlineOffset = tail.firstIndex(of: 0x0A) {
            let next = tail.index(after: newlineOffset)
            if next < tail.endIndex {
                trimmed = Data(tail[next...])
            } else {
                trimmed = tail
            }
        } else {
            trimmed = tail
        }

        try trimmed.write(to: url, options: .atomic)
    }

    private func snapshot() -> WidgetRotationDiagnosticsSnapshot {
        let sharedDefaults = UserDefaults(suiteName: appGroupID)
        let logURL = try? resolveLogURL()
        let path = logURL?.path ?? ""
        let logLines = readLogLines(at: logURL)
        let latestLogEntry = logLines.last
        let recentFailureCount24h = countRecentRotationFailures(in: logLines)
        let fileSize = logURL.flatMap { fileSizeBytes(at: $0) } ?? 0

        let lastFailureAt = sharedDefaults
            .flatMap { $0.string(forKey: lastFailureAtKey) }
            .flatMap { formatter.date(from: $0) }
        let lastSuccessAt = sharedDefaults
            .flatMap { $0.string(forKey: lastSuccessAtKey) }
            .flatMap { formatter.date(from: $0) }

        let status: WidgetRotationDiagnosticsSnapshot.RotationStatus
        switch (lastFailureAt, lastSuccessAt) {
        case (nil, nil):
            status = .noData
        case (nil, _):
            status = .ok
        case let (failure?, success?) where success >= failure:
            status = .ok
        default:
            status = .failed
        }

        return WidgetRotationDiagnosticsSnapshot(
            status: status,
            lastFailureAt: lastFailureAt,
            lastFailureDBMode: sharedDefaults?.string(forKey: lastFailureDBModeKey),
            lastFailureSQLiteCode: sharedDefaults?.object(forKey: lastFailureSQLiteCodeKey) as? Int,
            lastFailureSQLiteMessage: sharedDefaults?.string(forKey: lastFailureSQLiteMessageKey),
            lastFailureStage: sharedDefaults?.string(forKey: lastFailureStageKey),
            lastSuccessAt: lastSuccessAt,
            recentFailureCount24h: recentFailureCount24h,
            logFilePath: path,
            logFileSizeBytes: fileSize,
            latestLogEntry: latestLogEntry
        )
    }

    private func readLogLines(at url: URL?) -> [String] {
        guard let url,
              let data = try? Data(contentsOf: url),
              let text = String(data: data, encoding: .utf8) else {
            return []
        }

        return text
            .split(separator: "\n", omittingEmptySubsequences: true)
            .map(String.init)
    }

    private func fileSizeBytes(at url: URL) -> Int64 {
        let attributes = try? FileManager.default.attributesOfItem(atPath: url.path)
        return (attributes?[.size] as? NSNumber)?.int64Value ?? 0
    }

    private func countRecentRotationFailures(in lines: [String]) -> Int {
        let cutoff = Date().addingTimeInterval(-recentWindow)
        var count = 0
        for line in lines {
            guard let event = parseEvent(line),
                  let eventName = event["event"] as? String,
                  eventName == "rotation_stamp_failed",
                  let timestampText = event["timestamp"] as? String,
                  let timestamp = formatter.date(from: timestampText),
                  timestamp >= cutoff else {
                continue
            }
            count += 1
        }
        return count
    }

    private func parseEvent(_ line: String) -> [String: Any]? {
        guard let data = line.data(using: .utf8),
              let raw = try? JSONSerialization.jsonObject(with: data, options: []),
              let payload = raw as? [String: Any] else {
            return nil
        }
        return payload
    }

    private func eventIdentifier(in line: String) -> String? {
        parseEvent(line)?["event_id"] as? String
    }

    private func resolveLogURL() throws -> URL {
        guard let appSupportDirectory = FileManager.default.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        ).first else {
            throw NSError(
                domain: "AppDiagnosticsLog",
                code: 1,
                userInfo: [NSLocalizedDescriptionKey: "application support directory unavailable"]
            )
        }

        let diagnosticsDirectory = appSupportDirectory
            .appendingPathComponent("hyperlinked", isDirectory: true)
        try FileManager.default.createDirectory(
            at: diagnosticsDirectory,
            withIntermediateDirectories: true
        )
        return diagnosticsDirectory.appendingPathComponent("diagnostics.log", isDirectory: false)
    }
}
