import Foundation

enum WidgetRotationStampStatus {
    case healthy
    case paused
}

struct WidgetRotationFailureContext {
    let dbOpenMode: String
    let sqliteCode: Int32
    let sqliteMessage: String
    let stage: String
}

enum WidgetDiagnosticsBridge {
    private static let appGroupID = "group.fm.folder.hyperlinked"
    private static let pendingEventsKey = "diagnostics.pending_events.v1"
    private static let lastFailureAtKey = "diagnostics.widget_rotation.last_failure_at.v1"
    private static let lastFailureDBModeKey = "diagnostics.widget_rotation.last_failure_db_mode.v1"
    private static let lastFailureSQLiteCodeKey = "diagnostics.widget_rotation.last_failure_sqlite_code.v1"
    private static let lastFailureSQLiteMessageKey = "diagnostics.widget_rotation.last_failure_sqlite_message.v1"
    private static let lastFailureStageKey = "diagnostics.widget_rotation.last_failure_stage.v1"
    private static let lastSuccessAtKey = "diagnostics.widget_rotation.last_success_at.v1"
    private static let maxPendingEvents = 100
    private static let warningFreshnessWindow: TimeInterval = 24 * 60 * 60
    private static let formatter: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        formatter.timeZone = TimeZone(secondsFromGMT: 0)
        return formatter
    }()

    private static var sharedDefaults: UserDefaults? {
        UserDefaults(suiteName: appGroupID)
    }

    static func recordRotationStampSuccess(at date: Date = .now) {
        guard let sharedDefaults else {
            return
        }
        sharedDefaults.set(formatter.string(from: date), forKey: lastSuccessAtKey)
    }

    static func recordRotationStampFailure(
        _ context: WidgetRotationFailureContext,
        at date: Date = .now
    ) {
        guard let sharedDefaults else {
            return
        }

        let timestamp = formatter.string(from: date)
        sharedDefaults.set(timestamp, forKey: lastFailureAtKey)
        sharedDefaults.set(context.dbOpenMode, forKey: lastFailureDBModeKey)
        sharedDefaults.set(Int(context.sqliteCode), forKey: lastFailureSQLiteCodeKey)
        sharedDefaults.set(context.sqliteMessage, forKey: lastFailureSQLiteMessageKey)
        sharedDefaults.set(context.stage, forKey: lastFailureStageKey)

        var pendingEvents = sharedDefaults.stringArray(forKey: pendingEventsKey) ?? []
        pendingEvents.append(eventLine(timestamp: timestamp, context: context))
        if pendingEvents.count > maxPendingEvents {
            pendingEvents.removeFirst(pendingEvents.count - maxPendingEvents)
        }
        sharedDefaults.set(pendingEvents, forKey: pendingEventsKey)
    }

    static func rotationStampStatus(now: Date = .now) -> WidgetRotationStampStatus {
        guard let sharedDefaults else {
            return .healthy
        }

        guard let failureAtText = sharedDefaults.string(forKey: lastFailureAtKey),
              let failureAt = formatter.date(from: failureAtText) else {
            return .healthy
        }

        if now.timeIntervalSince(failureAt) > warningFreshnessWindow {
            return .healthy
        }

        if let successAtText = sharedDefaults.string(forKey: lastSuccessAtKey),
           let successAt = formatter.date(from: successAtText),
           successAt >= failureAt {
            return .healthy
        }

        return .paused
    }

    private static func eventLine(timestamp: String, context: WidgetRotationFailureContext) -> String {
        let payload: [String: Any] = [
            "event_id": UUID().uuidString,
            "timestamp": timestamp,
            "subsystem": "widget.local-cache",
            "event": "rotation_stamp_failed",
            "db_open_mode": context.dbOpenMode,
            "sqlite_code": Int(context.sqliteCode),
            "sqlite_message": context.sqliteMessage,
            "stage": context.stage,
        ]

        guard JSONSerialization.isValidJSONObject(payload),
              let data = try? JSONSerialization.data(withJSONObject: payload, options: []),
              let json = String(data: data, encoding: .utf8) else {
            return "{\"event\":\"rotation_stamp_failed\",\"timestamp\":\"\(timestamp)\"}"
        }

        return json
    }
}
