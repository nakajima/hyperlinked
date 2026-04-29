import Foundation
import OSLog

struct WidgetLocalStore {
    private static let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "fm.folder.hyperlinked.Widget",
        category: "local-cache"
    )

    func listHyperlinks(
        configuration: ConfigurationAppIntent,
        limit: Int,
        recordShown: Bool = true
    ) throws -> [WidgetHyperlink] {
        let store = try WidgetHyperlinkStore.openShared()
        let records = try store.listHyperlinks(
            configuration: configuration.widgetSelectionConfig,
            limit: limit
        )
        let hyperlinks = records.compactMap(WidgetHyperlink.init(record:))

        if recordShown && configuration.rotateSlowly {
            recordShownHyperlinks(hyperlinks.map(\.id))
        }

        return hyperlinks
    }

    func recordShownHyperlinks(_ hyperlinkIDs: [Int], at shownAt: Date = .now) {
        guard !hyperlinkIDs.isEmpty else {
            return
        }

        do {
            try WidgetHyperlinkStore.openShared().recordShownHyperlinks(hyperlinkIDs, at: shownAt)
            WidgetDiagnosticsBridge.recordRotationStampSuccess(at: shownAt)
        } catch {
            let failureContext = WidgetRotationFailureContext(
                dbOpenMode: "read_write",
                sqliteCode: -1,
                sqliteMessage: error.localizedDescription,
                stage: "grdb_write"
            )
            WidgetDiagnosticsBridge.recordRotationStampFailure(failureContext, at: shownAt)
            Self.logger.error(
                "Failed to stamp widget display metadata via GRDB: \(error.localizedDescription, privacy: .public)"
            )
        }
    }
}

private extension ConfigurationAppIntent {
    var widgetSelectionConfig: WidgetSelectionConfig {
        WidgetSelectionConfig(
            scope: resolvedWidgetSelectionScope,
            sortOrder: resolvedWidgetSelectionSortOrder,
            unclickedOnly: unclickedOnly
        )
    }

    private var resolvedWidgetSelectionScope: WidgetSelectionConfig.Scope {
        switch scope {
        case .saved:
            return .saved
        case .discoveredOnly:
            return .discoveredOnly
        case .all:
            return .all
        }
    }

    private var resolvedWidgetSelectionSortOrder: WidgetSelectionConfig.SortOrder {
        switch sortOrder {
        case .newest:
            return .newest
        case .oldest:
            return .oldest
        case .random:
            return .random
        }
    }
}

private extension WidgetHyperlink {
    init?(record: WidgetHyperlinkRecord) {
        guard let visitURL = URL(string: record.url) else {
            return nil
        }

        self.init(
            id: record.id,
            title: record.title,
            url: record.url,
            host: record.host,
            oneLiner: record.oneLiner,
            visitURL: visitURL,
            faviconURL: nil,
            thumbnailURL: record.thumbnailURL.flatMap(URL.init(string:)),
            thumbnailDarkURL: record.thumbnailDarkURL.flatMap(URL.init(string:)),
            fallbackColor: nil
        )
    }
}
