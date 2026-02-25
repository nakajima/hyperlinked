//
//  hyperlinkedTests.swift
//  hyperlinkedTests
//
//  Created by Pat Nakajima on 2/23/26.
//

import Testing
import Foundation
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

}
