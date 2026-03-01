import SwiftUI
import UIKit
import OSLog
import WidgetKit
import GRDBQuery

struct HyperlinksListView: View {
    @EnvironmentObject private var appModel: AppModel
    @Environment(\.colorScheme) private var colorScheme
    private static let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "fm.folder.hyperlinked",
        category: "hyperlink-cache"
    )

    @Query(CachedHyperlinksRequest(limit: 5000, rootOnly: false))
    private var cachedAllHyperlinks: [Hyperlink]
    @Query(CachedHyperlinksRequest(limit: 5000, rootOnly: true))
    private var cachedRootHyperlinks: [Hyperlink]
    @Query(PendingShareOutboxItemsRequest(limit: 200))
    private var pendingOutboxItems: [ShareOutboxItemRecord]
    @State private var isLoading = false
    @State private var errorMessage: String?
    @State private var activeSheet: ActiveSheet?
    @State private var isSearchPresented = false
    @State private var queryText = ""
    @AppStorage("hyperlinks.view_options.show_discovered_links")
    private var showDiscoveredLinks = false
    @AppStorage("hyperlinks.view_options.order_override")
    private var orderOverrideRawValue = ""

    private enum ActiveSheet: String, Identifiable {
        case add
        case settings

        var id: String { rawValue }
    }

    private enum ListRow: Identifiable {
        case pending(ShareOutboxItemRecord)
        case hyperlink(Hyperlink)

        var id: String {
            switch self {
            case .pending(let item):
                return "pending-\(item.id)"
            case .hyperlink(let hyperlink):
                return "hyperlink-\(hyperlink.id)"
            }
        }
    }

    private var trimmedQueryText: String {
        queryText.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var hasFreeText: Bool {
        !trimmedQueryText.isEmpty
    }

    private var hasEffectiveFilter: Bool {
        hasFreeText || showDiscoveredLinks
    }

    private var orderOverride: HyperlinkOrderFilter? {
        HyperlinkOrderFilter(rawValue: orderOverrideRawValue)
    }

    private var effectiveOrder: HyperlinkOrderFilter {
        orderOverride ?? (hasFreeText ? .relevance : .newest)
    }

    private var orderOptions: [HyperlinkOrderFilter] {
        if hasFreeText {
            return [.newest, .relevance, .oldest, .mostClicked, .recentlyClicked, .recentlyShownInWidget, .random]
        }
        return [.newest, .oldest, .mostClicked, .recentlyClicked, .recentlyShownInWidget, .random]
    }

    private var orderBinding: Binding<HyperlinkOrderFilter> {
        Binding(
            get: {
                effectiveOrder
            },
            set: { newValue in
                let defaultOrder: HyperlinkOrderFilter = hasFreeText ? .relevance : .newest
                orderOverrideRawValue = ((newValue == defaultOrder) ? nil : newValue)?.rawValue ?? ""
            }
        )
    }

    private var visibleHyperlinks: [Hyperlink] {
        let scoped = showDiscoveredLinks ? cachedAllHyperlinks : cachedRootHyperlinks
        let filtered = filterHyperlinks(scoped, query: trimmedQueryText)
        return sortHyperlinks(filtered, order: effectiveOrder, query: trimmedQueryText)
    }

    private var listRows: [ListRow] {
        pendingOutboxItems.map(ListRow.pending) + visibleHyperlinks.map(ListRow.hyperlink)
    }

    var body: some View {
        NavigationStack {
            listContent
            .navigationTitle("Hyperlinks")
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button {
                        activeSheet = .settings
                    } label: {
                        Label("Server", systemImage: "server.rack")
                    }
                }

                ToolbarItem(placement: .topBarTrailing) {
                    Menu {
                        Section("Sort") {
                            Picker("Sort", selection: orderBinding) {
                                ForEach(orderOptions) { option in
                                    Text(option.label).tag(option)
                                }
                            }
                        }

                        Toggle("Show discovered links", isOn: $showDiscoveredLinks)
                    } label: {
                        Label("View Options", systemImage: "line.3.horizontal.decrease.circle")
                    }
                    .disabled(appModel.apiClient == nil)
                }

                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        activeSheet = .add
                    } label: {
                        Image(systemName: "plus")
                    }
                    .disabled(appModel.apiClient == nil)
                }
            }
            .searchable(
                text: $queryText,
                isPresented: $isSearchPresented,
                placement: .navigationBarDrawer(displayMode: .automatic),
                prompt: "Enter a search query"
            )
        }
        .task(id: appModel.selectedServerURL?.absoluteString) {
            await loadHyperlinks()
        }
        .task(id: appModel.selectedServerURL?.absoluteString) {
            await retryPendingOutboxLoop()
        }
        .refreshable {
						WidgetCenter.shared.reloadAllTimelines()
            await loadHyperlinks()
        }
        .onSubmit(of: .search) {
            // Search is local over cached records.
        }
        .onChange(of: queryText) {
            if !hasFreeText, orderOverride == .relevance {
                orderOverrideRawValue = ""
            }
        }
        .onReceive(
            NotificationCenter.default.publisher(for: UIApplication.willEnterForegroundNotification)
        ) { _ in
            Task {
                await loadHyperlinks()
            }
        }
        .sheet(item: $activeSheet) { sheet in
            switch sheet {
            case .add:
                AddHyperlinkView { created in
                    persistHyperlinks(hyperlinks: [created])
                }
                .environmentObject(appModel)
            case .settings:
                ServerSettingsView(
                    pendingUploadsCount: pendingOutboxItems.count,
                    onChangeServer: {
                        activeSheet = nil
                        appModel.openServerSetup()
                    },
                    onRetryPendingUploads: {
                        Task {
                            await loadHyperlinks()
                        }
                    }
                )
                .environmentObject(appModel)
            }
        }
    }

    private var listContent: some View {
        List {
            if isLoading && visibleHyperlinks.isEmpty && pendingOutboxItems.isEmpty {
                Section {
                    HStack {
                        Spacer()
                        ProgressView("Loading hyperlinks…")
                        Spacer()
                    }
                    .padding(.vertical, 24)
                    .listRowSeparator(.hidden)
                }
            } else if let errorMessage, visibleHyperlinks.isEmpty && pendingOutboxItems.isEmpty {
                Section {
                    VStack(spacing: 12) {
                        Image(systemName: "wifi.slash")
                            .font(.system(size: 30))
                            .foregroundStyle(.secondary)
                        Text("Couldn’t Load Hyperlinks")
                            .font(.headline)
                        Text(errorMessage)
                            .multilineTextAlignment(.center)
                            .foregroundStyle(.secondary)
                        Button("Retry") {
                            Task {
                                await loadHyperlinks()
                            }
                        }
                        Button("Change Server") {
                            appModel.openServerSetup()
                        }
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 24)
                    .listRowSeparator(.hidden)
                }
            } else if visibleHyperlinks.isEmpty && pendingOutboxItems.isEmpty {
                Section {
                    VStack(spacing: 12) {
                        Image(systemName: "link.badge.plus")
                            .font(.system(size: 30))
                            .foregroundStyle(.secondary)
                        Text(hasEffectiveFilter ? "No Matching Hyperlinks" : "No Hyperlinks Yet")
                            .font(.headline)
                        Text(
                            hasEffectiveFilter
                                ? "Try changing your filters."
                                : "Add one with the plus button."
                        )
                        .multilineTextAlignment(.center)
                        .foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 24)
                    .listRowSeparator(.hidden)
                }
            } else {
                Section {
                    ForEach(listRows) { row in
                        switch row {
                        case .pending(let pendingItem):
                            PendingOutboxRowContent(item: pendingItem)
                        case .hyperlink(let hyperlink):
                            NavigationLink(
                                destination: HyperlinkDetailView(hyperlinkID: hyperlink.id, fallback: hyperlink)
                            ) {
                                HyperlinkListRowContent(
                                    hyperlink: hyperlink,
                                    colorScheme: colorScheme
                                )
                            }
                        }
                    }
                }
            }
        }
        .listStyle(.plain)
    }

    private func loadHyperlinks() async {
        guard let client = appModel.apiClient else {
            errorMessage = "No server selected."
            return
        }

        isLoading = true
        defer { isLoading = false }

        do {
            await retryPendingOutbox(using: client)
            let fetched = try await client.listHyperlinks()
            persistHyperlinks(hyperlinks: fetched)
            errorMessage = nil
        } catch is CancellationError {
            return
        } catch let urlError as URLError where urlError.code == .cancelled {
            return
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func retryPendingOutboxLoop() async {
        while !Task.isCancelled {
            guard let client = appModel.apiClient else {
                return
            }
            await retryPendingOutbox(using: client)
            try? await Task.sleep(nanoseconds: 30_000_000_000)
        }
    }

    private func retryPendingOutbox(using client: APIClient) async {
        guard let store = try? ShareOutboxStore.openShared() else {
            return
        }
        let coordinator = OutboxDeliveryCoordinator(store: store, client: client)
        _ = await coordinator.drainDueItems(limit: 20)
    }

    private func persistHyperlinks(hyperlinks: [Hyperlink]) {
        guard !hyperlinks.isEmpty else {
            return
        }

        do {
            let store = try HyperlinkStore.openShared()
            try store.upsert(hyperlinks: hyperlinks)
        } catch {
            Self.logger.debug("Failed to persist hyperlinks: \(error.localizedDescription, privacy: .public)")
        }
    }

    private func filterHyperlinks(_ hyperlinks: [Hyperlink], query: String) -> [Hyperlink] {
        let tokens = query
            .lowercased()
            .split(whereSeparator: \.isWhitespace)
            .map(String.init)
        guard !tokens.isEmpty else {
            return hyperlinks
        }

        return hyperlinks.filter { hyperlink in
            let host = URL(string: hyperlink.url)?.host?.lowercased() ?? ""
            let haystacks = [
                hyperlink.title.lowercased(),
                hyperlink.url.lowercased(),
                hyperlink.rawURL.lowercased(),
                host,
            ]
            return tokens.allSatisfy { token in
                haystacks.contains { $0.contains(token) }
            }
        }
    }

    private func sortHyperlinks(
        _ hyperlinks: [Hyperlink],
        order: HyperlinkOrderFilter,
        query: String
    ) -> [Hyperlink] {
        switch order {
        case .newest:
            return hyperlinks.sorted(by: newestFirst)
        case .oldest:
            return hyperlinks.sorted(by: oldestFirst)
        case .mostClicked:
            return hyperlinks.sorted(by: mostClickedFirst)
        case .recentlyClicked:
            return hyperlinks.sorted(by: recentlyClickedFirst)
        case .recentlyShownInWidget:
            return hyperlinks.sorted(by: recentlyShownInWidgetFirst)
        case .random:
            return randomlyOrdered(hyperlinks, querySeed: query)
        case .relevance:
            return relevanceOrdered(hyperlinks, query: query)
        }
    }

    private func relevanceOrdered(_ hyperlinks: [Hyperlink], query: String) -> [Hyperlink] {
        let tokens = query
            .lowercased()
            .split(whereSeparator: \.isWhitespace)
            .map(String.init)
        guard !tokens.isEmpty else {
            return hyperlinks.sorted(by: newestFirst)
        }

        return hyperlinks.sorted { lhs, rhs in
            let leftScore = relevanceScore(hyperlink: lhs, tokens: tokens)
            let rightScore = relevanceScore(hyperlink: rhs, tokens: tokens)
            if leftScore != rightScore {
                return leftScore > rightScore
            }
            return newestFirst(lhs: lhs, rhs: rhs)
        }
    }

    private func relevanceScore(hyperlink: Hyperlink, tokens: [String]) -> Int {
        let title = hyperlink.title.lowercased()
        let url = hyperlink.url.lowercased()
        let rawURL = hyperlink.rawURL.lowercased()
        let host = URL(string: hyperlink.url)?.host?.lowercased() ?? ""

        var score = 0
        for token in tokens {
            if title.contains(token) {
                score += 6
            }
            if host.contains(token) {
                score += 4
            }
            if url.contains(token) {
                score += 2
            }
            if rawURL.contains(token) {
                score += 1
            }
        }
        return score
    }

    private func randomlyOrdered(_ hyperlinks: [Hyperlink], querySeed: String) -> [Hyperlink] {
        let seed = stableSeed(from: querySeed)
        return hyperlinks.sorted { lhs, rhs in
            let lhsRank = (lhs.id &* 1_103_515_245) ^ seed
            let rhsRank = (rhs.id &* 1_103_515_245) ^ seed
            if lhsRank != rhsRank {
                return lhsRank < rhsRank
            }
            return newestFirst(lhs: lhs, rhs: rhs)
        }
    }

    private func stableSeed(from value: String) -> Int {
        var hash = 2_166_136_261
        for scalar in value.unicodeScalars {
            hash ^= Int(scalar.value)
            hash = hash &* 16_777_619
        }
        return hash
    }

    private func newestFirst(lhs: Hyperlink, rhs: Hyperlink) -> Bool {
        if lhs.createdAt != rhs.createdAt {
            return lhs.createdAt > rhs.createdAt
        }
        return lhs.id > rhs.id
    }

    private func oldestFirst(lhs: Hyperlink, rhs: Hyperlink) -> Bool {
        if lhs.createdAt != rhs.createdAt {
            return lhs.createdAt < rhs.createdAt
        }
        return lhs.id < rhs.id
    }

    private func mostClickedFirst(lhs: Hyperlink, rhs: Hyperlink) -> Bool {
        if lhs.clicksCount != rhs.clicksCount {
            return lhs.clicksCount > rhs.clicksCount
        }
        return newestFirst(lhs: lhs, rhs: rhs)
    }

    private func recentlyClickedFirst(lhs: Hyperlink, rhs: Hyperlink) -> Bool {
        let lhsLastClicked = lhs.lastClickedAt ?? ""
        let rhsLastClicked = rhs.lastClickedAt ?? ""
        if lhsLastClicked != rhsLastClicked {
            return lhsLastClicked > rhsLastClicked
        }
        return newestFirst(lhs: lhs, rhs: rhs)
    }

    private func recentlyShownInWidgetFirst(lhs: Hyperlink, rhs: Hyperlink) -> Bool {
        switch (lhs.lastShownInWidget, rhs.lastShownInWidget) {
        case let (.some(lhsShownAt), .some(rhsShownAt)):
            if lhsShownAt != rhsShownAt {
                return lhsShownAt > rhsShownAt
            }
        case (.some, .none):
            return true
        case (.none, .some):
            return false
        case (.none, .none):
            break
        }
        return newestFirst(lhs: lhs, rhs: rhs)
    }
}

private enum HyperlinkOrderFilter: String, Identifiable {
    case newest
    case relevance
    case oldest
    case mostClicked = "most-clicked"
    case recentlyClicked = "recently-clicked"
    case recentlyShownInWidget = "recently-shown-in-widget"
    case random

    var id: String { rawValue }

    var label: String {
        switch self {
        case .newest:
            return "Newest"
        case .relevance:
            return "Relevance"
        case .oldest:
            return "Oldest"
        case .mostClicked:
            return "Most Clicked"
        case .recentlyClicked:
            return "Recently Clicked"
        case .recentlyShownInWidget:
            return "Recently Shown in Widget"
        case .random:
            return "Random"
        }
    }
}

private struct HyperlinkListRowContent: View {
    let hyperlink: Hyperlink
    let colorScheme: ColorScheme

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            HyperlinkThumbnailView(hyperlink: hyperlink, colorScheme: colorScheme)

            VStack(alignment: .leading, spacing: 4) {
                Text(hyperlink.title)
                    .font(.headline)
                    .lineLimit(2)
                Text(hyperlink.url)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
										.lineLimit(1)
							
							if let description = hyperlink.ogDescription {
								Text(description)
									.foregroundStyle(.secondary)
									.font(.caption)
									.lineLimit(2)
							}

                HStack(spacing: 12) {
                    if let parent = hyperlink.discoveredVia.first {
                        let discoveredLabel = parent.title.trimmingCharacters(
                            in: .whitespacesAndNewlines
                        ).isEmpty ? parent.url : parent.title
                        Text("Discovered via \(discoveredLabel)")
                            .foregroundStyle(.secondary)
                    }
  
                    if hyperlink.isURLValid == false {
                        Text("Invalid URL")
                            .foregroundStyle(.orange)
                    }
                }
                .font(.caption2)
                .foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 4)
    }
}

private struct PendingOutboxRowContent: View {
    let item: ShareOutboxItemRecord

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text(item.title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? item.url : item.title)
                    .font(.headline)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                Text(item.url)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                HStack(spacing: 12) {
                    Text("Pending upload")
                    if item.attemptCount > 0 {
                        Text("Retries \(item.attemptCount)")
                    }
                }
                .font(.caption)
                .foregroundStyle(.tertiary)
                if let lastError = item.lastError,
                   !lastError.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    Text(lastError)
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                        .lineLimit(1)
                }
            }
        }
        .padding(.vertical, 4)
        .opacity(0.78)
    }
}

#if DEBUG
struct HyperlinksListRowPreviews: PreviewProvider {
    static var previews: some View {
        List {
            HyperlinkListRowContent(
                hyperlink: sampleHyperlink(),
                colorScheme: .light
            )
            HyperlinkListRowContent(
                hyperlink: sampleInvalidHyperlink(),
                colorScheme: .dark
            )
            HyperlinkListRowContent(
                hyperlink: sampleDiscoveredHyperlink(),
                colorScheme: .light
            )
            PendingOutboxRowContent(item: samplePendingOutboxItem())
        }
        .listStyle(.plain)
        .previewDisplayName("Hyperlink List Rows")
    }

    private static func sampleHyperlink() -> Hyperlink {
        Hyperlink(
            id: 1,
            title: "@LiveModel `in SwiftData",
            url: "https://patschbewebblog.com/posts/2-live-model/",
            rawURL: "https://patschbewebblog.com/posts/2-live-model/",
            ogDescription: "A practical walkthrough of live models in SwiftData.",
            isURLValid: true,
            discoveryDepth: 0,
            clicksCount: 2,
            lastClickedAt: "2026-02-28T01:45:00Z",
            processingState: "ready",
            createdAt: "2026-02-27T12:00:00Z",
            updatedAt: "2026-02-28T01:45:00Z",
            thumbnailURL: nil,
            thumbnailDarkURL: nil,
            screenshotURL: nil,
            screenshotDarkURL: nil
        )
    }

    private static func sampleInvalidHyperlink() -> Hyperlink {
        Hyperlink(
            id: 2,
            title: "Imported Link Missing Scheme",
            url: "example.com/no-scheme",
            rawURL: "example.com/no-scheme",
            ogDescription: nil,
            isURLValid: false,
            discoveryDepth: 0,
            clicksCount: 0,
            lastClickedAt: nil,
            processingState: "ready",
            createdAt: "2026-02-27T12:00:00Z",
            updatedAt: "2026-02-28T01:45:00Z",
            thumbnailURL: nil,
            thumbnailDarkURL: nil,
            screenshotURL: nil,
            screenshotDarkURL: nil
        )
    }

    private static func sampleDiscoveredHyperlink() -> Hyperlink {
        Hyperlink(
            id: 3,
            title: "SwiftUI Data Flow Notes",
            url: "https://developer.apple.com/documentation/swiftui/data-flow",
            rawURL: "https://developer.apple.com/documentation/swiftui/data-flow",
            ogDescription: "Apple docs page found while crawling a root link.",
            isURLValid: true,
            discoveryDepth: 1,
            clicksCount: 7,
            lastClickedAt: "2026-02-28T01:40:00Z",
            processingState: "ready",
            createdAt: "2026-02-27T14:00:00Z",
            updatedAt: "2026-02-28T01:40:00Z",
            thumbnailURL: nil,
            thumbnailDarkURL: nil,
            screenshotURL: nil,
            screenshotDarkURL: nil,
            discoveredVia: [
                HyperlinkRef(
                    id: 1,
                    title: "@LiveModel `in SwiftData",
                    url: "https://patschbewebblog.com/posts/2-live-model/",
                    rawURL: "https://patschbewebblog.com/posts/2-live-model/"
                )
            ]
        )
    }

    private static func samplePendingOutboxItem() -> ShareOutboxItemRecord {
        ShareOutboxItemRecord(
            id: "preview-pending-1",
            url: "https://ryanbrewer.dev/posts/sequent-calculus/",
            title: "Par Part 1: Sequent Calculus",
            createdAt: Date().timeIntervalSince1970 - 120,
            state: ShareOutboxState.pending.rawValue,
            attemptCount: 1,
            nextAttemptAt: Date().timeIntervalSince1970 + 10,
            lastAttemptAt: Date().timeIntervalSince1970 - 30,
            lastError: "Temporary network timeout",
            deliveredAt: nil
        )
    }
}
#endif
