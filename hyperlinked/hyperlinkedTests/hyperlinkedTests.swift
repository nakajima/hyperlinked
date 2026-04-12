//
//  hyperlinkedTests.swift
//  hyperlinkedTests
//
//  Created by Pat Nakajima on 2/23/26.
//

import Testing
import Foundation
import GRDB
@testable import hyperlinked

@MainActor
struct hyperlinkedTests {

    @Test
    func normalizesManualServerURL() {
        let normalized = AppModel.normalizedServerURL(from: "192.168.1.5:8765/hyperlinks?q=test")
        #expect(normalized?.absoluteString == "http://192.168.1.5:8765")
    }

    @Test
    func rejectsInvalidServerURL() {
        let normalized = AppModel.normalizedServerURL(from: "not a url")
        #expect(normalized == nil)
    }

    @Test
    func decodesHyperlinkFromJSON() throws {
        let payload = """
        {
          "id": 42,
          "title": "Example",
          "url": "https://example.com",
          "raw_url": "https://example.com/?utm_source=test",
          "clicks_count": 2,
          "last_clicked_at": null,
          "processing_state": "idle",
          "created_at": "2026-02-22 10:00:00",
          "updated_at": "2026-02-22 11:00:00"
        }
        """.data(using: .utf8)!

        let decoded = try JSONDecoder().decode(Hyperlink.self, from: payload)
        #expect(decoded.id == 42)
        #expect(decoded.rawURL == "https://example.com/?utm_source=test")
        #expect(decoded.processingState == "idle")
        #expect(decoded.lastShownInWidget == nil)
    }

    @Test
    func decodesHyperlinkWithLastShownInWidget() throws {
        let payload = """
        {
          "id": 7,
          "title": "Example",
          "url": "https://example.com",
          "raw_url": "https://example.com",
          "clicks_count": 0,
          "last_clicked_at": null,
          "processing_state": "ready",
          "created_at": "2026-02-22T10:00:00Z",
          "updated_at": "2026-02-22T11:00:00Z",
          "last_shown_in_widget": "2026-03-01T09:40:00Z"
        }
        """.data(using: .utf8)!

        let decoded = try JSONDecoder().decode(Hyperlink.self, from: payload)
        #expect(decoded.lastShownInWidget == "2026-03-01T09:40:00Z")
    }

    @Test
    func listQueryDefaultsToRootScope() {
        let query = HyperlinksListQueryBuilder.build(
            queryText: "",
            showDiscoveredLinks: false,
            orderOverrideRawValue: nil
        )
        #expect(query == "scope:root")
    }

    @Test
    func listQueryUsesAllScopeWhenShowingDiscovered() {
        let query = HyperlinksListQueryBuilder.build(
            queryText: "",
            showDiscoveredLinks: true,
            orderOverrideRawValue: nil
        )
        #expect(query == "scope:all")
    }

    @Test
    func listQueryIncludesTrimmedFreeTextAndOrderOverride() {
        let query = HyperlinksListQueryBuilder.build(
            queryText: "  rust links ",
            showDiscoveredLinks: false,
            orderOverrideRawValue: "most-clicked"
        )
        #expect(query == "scope:root rust links order:most-clicked")
    }

    @Test
    func listQueryEmitsSingleScopeToken() {
        let query = HyperlinksListQueryBuilder.build(
            queryText: "swift",
            showDiscoveredLinks: true,
            orderOverrideRawValue: "oldest"
        )
        let scopeTokens = query
            .split(separator: " ")
            .map(String.init)
            .filter { $0.hasPrefix("scope:") }
        #expect(scopeTokens == ["scope:all"])
    }

    @Test
    func basicAuthCredentialsBuildAuthorizationHeader() {
        let credentials = BasicAuthCredentials(
            username: " alice ",
            password: "s3cr3t"
        )
        #expect(credentials.authorizationHeaderValue == "Basic YWxpY2U6czNjcjN0")
    }

    @Test
    func detectsPDFHyperlinksFromURL() {
        let hyperlink = Hyperlink(
            id: 9,
            title: "Paper",
            url: "https://example.com/files/paper.pdf",
            rawURL: "https://example.com/files/paper.pdf?download=1",
            ogDescription: nil,
            isURLValid: true,
            discoveryDepth: 0,
            clicksCount: 0,
            lastClickedAt: nil,
            processingState: "ready",
            createdAt: "2026-03-05T00:00:00Z",
            updatedAt: "2026-03-05T00:00:00Z",
            thumbnailURL: nil,
            thumbnailDarkURL: nil,
            screenshotURL: nil,
            screenshotDarkURL: nil,
            discoveredVia: []
        )

        #expect(hyperlink.looksLikePDF)
    }

    @Test
    func serverCredentialKeyUsesNormalizedServerURL() {
        let url = URL(string: "HTTP://Example.com:8765/hyperlinks?q=1#frag")!
        #expect(AppModel.serverCredentialKey(for: url) == "http://example.com:8765")
    }

    @Test
    func upsertNonEmptyReloadsWidgetTimeline() throws {
        let (store, reloader, _) = try makeStoreForWidgetReloadTesting()

        try store.upsert(hyperlinks: [makeHyperlink(id: 1)])

        #expect(reloader.reloadCount == 1)
    }

    @Test
    func upsertEmptyDoesNotReloadWidgetTimeline() throws {
        let (store, reloader, _) = try makeStoreForWidgetReloadTesting()

        try store.upsert(hyperlinks: [])

        #expect(reloader.reloadCount == 0)
    }

    @Test
    func applyNonEmptyBatchReloadsWidgetTimeline() throws {
        let (store, reloader, _) = try makeStoreForWidgetReloadTesting()
        let batch = UpdatedHyperlinksBatch(
            serverUpdatedAt: "2026-03-05T00:00:00Z",
            changes: [
                UpdatedHyperlinkChange(
                    id: 1,
                    changeType: .updated,
                    updatedAt: "2026-03-05T00:00:00Z",
                    hyperlink: makeHyperlink(id: 1)
                ),
            ]
        )

        try store.apply(updatedBatch: batch)

        #expect(reloader.reloadCount == 1)
    }

    @Test
    func applyEmptyBatchDoesNotReloadWidgetTimeline() throws {
        let (store, reloader, _) = try makeStoreForWidgetReloadTesting()
        let batch = UpdatedHyperlinksBatch(
            serverUpdatedAt: "2026-03-05T00:00:00Z",
            changes: []
        )

        try store.apply(updatedBatch: batch)

        #expect(reloader.reloadCount == 0)
    }

    @Test
    func clearAllReloadsWidgetTimeline() throws {
        let (store, reloader, _) = try makeStoreForWidgetReloadTesting()

        try store.clearAll()

        #expect(reloader.reloadCount == 1)
    }

    @Test
    func replaceAllRemovesDeletedRowsAndReloadsWidgetTimeline() throws {
        let (store, reloader, dbQueue) = try makeStoreForWidgetReloadTesting()

        try store.upsert(hyperlinks: [makeHyperlink(id: 1), makeHyperlink(id: 2)])
        reloader.reset()

        try store.replaceAll(hyperlinks: [makeHyperlink(id: 2), makeHyperlink(id: 3)])

        let persistedIDs = try dbQueue.read {
            try Int.fetchAll($0, sql: "SELECT id FROM \(DB.hyperlinkTableName) ORDER BY id ASC")
        }
        #expect(persistedIDs == [2, 3])
        #expect(reloader.reloadCount == 1)
    }

    @Test
    func replaceAllEmptyClearsRowsAndReloadsWidgetTimeline() throws {
        let (store, reloader, dbQueue) = try makeStoreForWidgetReloadTesting()

        try store.upsert(hyperlinks: [makeHyperlink(id: 1)])
        reloader.reset()

        try store.replaceAll(hyperlinks: [])

        let persistedCount = try dbQueue.read {
            try Int.fetchOne($0, sql: "SELECT COUNT(*) FROM \(DB.hyperlinkTableName)") ?? 0
        }
        #expect(persistedCount == 0)
        #expect(reloader.reloadCount == 1)
    }

    @Test
    func offlineSnapshotStorePersistsReadabilityStatus() throws {
        let dbQueue = try DatabaseQueue(path: ":memory:")
        try dbQueue.write { db in
            try db.create(table: DB.hyperlinkOfflineSnapshotTableName) { t in
                t.column("hyperlink_id", .integer).primaryKey()
                t.column("readability_state", .text).notNull().defaults(to: "missing")
                t.column("readability_path", .text)
                t.column("readability_error", .text)
                t.column("readability_saved_at", .text)
                t.column("pdf_state", .text).notNull().defaults(to: "missing")
                t.column("pdf_path", .text)
                t.column("pdf_error", .text)
                t.column("pdf_saved_at", .text)
            }
        }

        let tempDirectory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        let store = HyperlinkOfflineStore(dbQueue: dbQueue, fileManager: .default)

        try FileManager.default.createDirectory(at: tempDirectory, withIntermediateDirectories: true)
        let markdownURL = tempDirectory.appendingPathComponent("readability.md")
        try "# Example".write(to: markdownURL, atomically: true, encoding: .utf8)

        try store.upsert(
            HyperlinkOfflineSnapshot(
                hyperlinkID: 12,
                readabilityState: .available,
                readabilityPath: markdownURL.path,
                readabilityError: nil,
                readabilitySavedAt: "2026-03-20T00:00:00Z"
            )
        )

        let snapshot = try store.snapshot(for: 12)
        #expect(snapshot.resolvedReadabilityState == .available)
        #expect(snapshot.readabilityFileURL?.path == markdownURL.path)
    }

    @Test
    func saveReadabilityHTMLPersistsHTMLSnapshotPath() throws {
        let dbQueue = try DatabaseQueue(path: ":memory:")
        try dbQueue.write { db in
            try db.create(table: DB.hyperlinkOfflineSnapshotTableName) { t in
                t.column("hyperlink_id", .integer).primaryKey()
                t.column("readability_state", .text).notNull().defaults(to: "missing")
                t.column("readability_path", .text)
                t.column("readability_error", .text)
                t.column("readability_saved_at", .text)
                t.column("pdf_state", .text).notNull().defaults(to: "missing")
                t.column("pdf_path", .text)
                t.column("pdf_error", .text)
                t.column("pdf_saved_at", .text)
            }
        }

        let store = HyperlinkOfflineStore(dbQueue: dbQueue, fileManager: .default)
        try store.saveReadabilityHTML("<html><body>Readable HTML</body></html>", hyperlinkID: 13)

        let snapshot = try store.snapshot(for: 13)
        #expect(snapshot.resolvedReadabilityState == .available)
        #expect(snapshot.readabilityFileURL?.pathExtension == "html")
    }

    @Test
    func readabilityHTMLUpgradeRetryPlanRetriesOnlyPDFs() {
        let pdf = Hyperlink(
            id: 21,
            title: "Paper",
            url: "https://example.com/paper.pdf",
            rawURL: "https://example.com/paper.pdf",
            summary: nil,
            ogDescription: nil,
            isURLValid: true,
            discoveryDepth: 0,
            clicksCount: 0,
            lastClickedAt: nil,
            processingState: "ready",
            createdAt: "2026-04-11T00:00:00Z",
            updatedAt: "2026-04-11T00:00:00Z",
            thumbnailURL: nil,
            thumbnailDarkURL: nil,
            screenshotURL: nil,
            screenshotDarkURL: nil,
            discoveredVia: []
        )
        let article = Hyperlink(
            id: 22,
            title: "Article",
            url: "https://example.com/article",
            rawURL: "https://example.com/article",
            summary: nil,
            ogDescription: nil,
            isURLValid: true,
            discoveryDepth: 0,
            clicksCount: 0,
            lastClickedAt: nil,
            processingState: "ready",
            createdAt: "2026-04-11T00:00:00Z",
            updatedAt: "2026-04-11T00:00:00Z",
            thumbnailURL: nil,
            thumbnailDarkURL: nil,
            screenshotURL: nil,
            screenshotDarkURL: nil,
            discoveredVia: []
        )

        #expect(ReadabilityHTMLUpgradeRetryPlan.shouldRetry(for: pdf))
        #expect(!ReadabilityHTMLUpgradeRetryPlan.shouldRetry(for: article))
    }

    @Test
    func readabilityHTMLUpgradeRetryPlanUsesExpectedBackoff() {
        #expect(ReadabilityHTMLUpgradeRetryPlan.retryDelaySeconds == [2, 4, 8, 16])
    }

    @Test
    func readabilityHTMLDocumentStylerInjectsDarkModeImageTreatmentIntoHead() {
        let styled = ReadabilityHTMLDocumentStyler.styledHTML(
            from: "<html><head><title>Doc</title></head><body><img src='figure.png'><mjx-container class='MathJax'></mjx-container></body></html>"
        )

        #expect(styled.contains("hyperlinked-readable-html-theme"))
        #expect(styled.contains("prefers-color-scheme: dark"))
        #expect(styled.contains("filter: brightness(0.58) contrast(0.92) saturate(0.88);"))
        #expect(styled.contains("mjx-container *"))
        #expect(styled.contains("<img src='figure.png'>"))
    }

    @Test
    func readabilityHTMLDocumentStylerWrapsFragmentsInFullHTMLDocument() {
        let styled = ReadabilityHTMLDocumentStyler.styledHTML(
            from: "<article><img src='figure.png'></article>"
        )

        #expect(styled.contains("<!DOCTYPE html>"))
        #expect(styled.contains("<body>"))
        #expect(styled.contains("<article><img src='figure.png'></article>"))
    }

    private func makeStoreForWidgetReloadTesting() throws -> (HyperlinkStore, WidgetTimelineReloaderSpy, DatabaseQueue) {
        let reloader = WidgetTimelineReloaderSpy()
        let dbQueue = try DatabaseQueue(path: ":memory:")
        try dbQueue.write { db in
            try db.create(table: DB.hyperlinkTableName) { t in
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
                t.column("last_shown_in_widget", .text)
                t.column("thumbnail_url", .text)
                t.column("thumbnail_dark_url", .text)
                t.column("screenshot_url", .text)
                t.column("screenshot_dark_url", .text)
                t.column("discovered_via_json", .text).notNull().defaults(to: "[]")
            }
        }
        let store = HyperlinkStore(dbQueue: dbQueue, timelineReloader: reloader)
        return (store, reloader, dbQueue)
    }

    private func makeHyperlink(id: Int) -> Hyperlink {
        Hyperlink(
            id: id,
            title: "Example \(id)",
            url: "https://example.com/\(id)",
            rawURL: "https://example.com/\(id)",
            ogDescription: "Example description",
            isURLValid: true,
            discoveryDepth: 0,
            clicksCount: 0,
            lastClickedAt: nil,
            processingState: "ready",
            createdAt: "2026-03-05T00:00:00Z",
            updatedAt: "2026-03-05T00:00:00Z",
            thumbnailURL: nil,
            thumbnailDarkURL: nil,
            screenshotURL: nil,
            screenshotDarkURL: nil,
            discoveredVia: []
        )
    }

}

private final class WidgetTimelineReloaderSpy: WidgetTimelineReloading {
    private(set) var reloadCount = 0

    func reloadHyperlinksWidgetTimeline() {
        reloadCount += 1
    }

    func reset() {
        reloadCount = 0
    }
}
