import SwiftUI
import UIKit
import OSLog
import GRDBQuery

struct HyperlinksListView: View {
    @EnvironmentObject private var appModel: AppModel
    @Environment(\.colorScheme) private var colorScheme
    private let diagnosticsLogger = AppEventLogger(component: "HyperlinksListView")
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
    @State private var latestServerUpdatedAt: String?
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
            latestServerUpdatedAt = nil
            diagnosticsLogger.log(
                "hyperlinks_list_server_context_changed",
                details: ["selected_server": appModel.selectedServerURL?.absoluteString ?? "none"]
            )
            await loadHyperlinks()
        }
        .task(id: appModel.selectedServerURL?.absoluteString) {
            await retryPendingOutboxLoop()
        }
        .task(id: appModel.selectedServerURL?.absoluteString) {
            await refreshUpdatedHyperlinksLoop()
        }
        .refreshable {
            diagnosticsLogger.log("hyperlinks_list_refresh_requested")
            await loadHyperlinks()
            appModel.refreshDiagnostics()
        }
        .onSubmit(of: .search) {
            // Search is local over cached records.
        }
        .onChange(of: queryText) {
            diagnosticsLogger.log(
                "hyperlinks_query_changed",
                details: [
                    "query_length": String(trimmedQueryText.count),
                    "has_free_text": hasFreeText ? "true" : "false",
                ]
            )
            if !hasFreeText, orderOverride == .relevance {
                orderOverrideRawValue = ""
            }
        }
        .onChange(of: appModel.selectedServerURL?.absoluteString) {
            latestServerUpdatedAt = nil
        }
        .onReceive(
            NotificationCenter.default.publisher(for: UIApplication.willEnterForegroundNotification)
        ) { _ in
            diagnosticsLogger.log("hyperlinks_list_will_enter_foreground")
            appModel.refreshDiagnostics()
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
            diagnosticsLogger.log(
                "load_hyperlinks_skipped",
                details: ["reason": "missing_api_client"]
            )
            return
        }

        isLoading = true
        diagnosticsLogger.log(
            "load_hyperlinks_started",
            details: [
                "selected_server": appModel.selectedServerURL?.absoluteString ?? "none",
                "pending_outbox_count": String(pendingOutboxItems.count),
            ]
        )
        defer { isLoading = false }

        do {
            await retryPendingOutbox(using: client)
            let fetched = try await client.listHyperlinks()
            replaceCachedHyperlinks(hyperlinks: fetched)
            appModel.startOfflineBackfillIfNeeded(force: true)
            latestServerUpdatedAt = newestUpdatedAt(in: fetched) ?? latestServerUpdatedAt
            errorMessage = nil
            diagnosticsLogger.log(
                "load_hyperlinks_succeeded",
                details: [
                    "fetched_count": String(fetched.count),
                    "latest_server_updated_at": latestServerUpdatedAt ?? "none",
                ]
            )
        } catch is CancellationError {
            diagnosticsLogger.log("load_hyperlinks_cancelled", details: ["reason": "task_cancelled"])
            return
        } catch let urlError as URLError where urlError.code == .cancelled {
            diagnosticsLogger.log("load_hyperlinks_cancelled", details: ["reason": "url_session_cancelled"])
            return
        } catch {
            errorMessage = error.localizedDescription
            diagnosticsLogger.logError("load_hyperlinks_failed", error: error)
        }
    }

    private func refreshUpdatedHyperlinksLoop() async {
        while !Task.isCancelled {
            guard let client = appModel.apiClient else {
                return
            }

            guard let cursor = latestServerUpdatedAt ?? newestUpdatedAt(in: cachedAllHyperlinks) else {
                try? await Task.sleep(nanoseconds: 30_000_000_000)
                continue
            }

            do {
                let batch = try await client.fetchUpdatedHyperlinks(updatedAt: cursor)
                applyUpdatedHyperlinks(batch)
                appModel.startOfflineBackfillIfNeeded(force: true)
                latestServerUpdatedAt = batch.serverUpdatedAt
                diagnosticsLogger.log(
                    "updated_hyperlinks_refresh_succeeded",
                    details: [
                        "cursor": cursor,
                        "change_count": String(batch.changes.count),
                        "server_updated_at": batch.serverUpdatedAt,
                    ]
                )
            } catch is CancellationError {
                diagnosticsLogger.log("updated_hyperlinks_refresh_cancelled")
                return
            } catch let urlError as URLError where urlError.code == .cancelled {
                diagnosticsLogger.log("updated_hyperlinks_refresh_cancelled", details: ["reason": "url_session_cancelled"])
                return
            } catch {
                diagnosticsLogger.logError(
                    "updated_hyperlinks_refresh_failed",
                    error: error,
                    details: ["cursor": cursor]
                )
                Self.logger.debug(
                    "Failed to refresh updated hyperlinks: \(error.localizedDescription, privacy: .public)"
                )
            }

            try? await Task.sleep(nanoseconds: 30_000_000_000)
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
            diagnosticsLogger.log(
                "pending_outbox_retry_skipped",
                details: ["reason": "store_open_failed"]
            )
            return
        }
        let coordinator = OutboxDeliveryCoordinator(store: store, client: client)
        let result = await coordinator.drainDueItems(limit: 20)
        if result.attempted > 0 || result.failed > 0 {
            diagnosticsLogger.log(
                "pending_outbox_retry_completed",
                details: [
                    "attempted": String(result.attempted),
                    "delivered": String(result.delivered),
                    "failed": String(result.failed),
                ]
            )
        }
    }

    private func replaceCachedHyperlinks(hyperlinks: [Hyperlink]) {
        do {
            let store = try HyperlinkStore.openShared()
            try store.replaceAll(hyperlinks: hyperlinks)
        } catch {
            diagnosticsLogger.logError(
                "replace_cached_hyperlinks_failed",
                error: error,
                details: ["hyperlink_count": String(hyperlinks.count)]
            )
            Self.logger.debug(
                "Failed to replace cached hyperlinks: \(error.localizedDescription, privacy: .public)"
            )
        }
    }

    private func persistHyperlinks(hyperlinks: [Hyperlink]) {
        guard !hyperlinks.isEmpty else {
            diagnosticsLogger.log(
                "persist_hyperlinks_skipped",
                details: ["reason": "empty_batch"]
            )
            return
        }

        do {
            let store = try HyperlinkStore.openShared()
            try store.upsert(hyperlinks: hyperlinks)
        } catch {
            diagnosticsLogger.logError(
                "persist_hyperlinks_failed",
                error: error,
                details: ["hyperlink_count": String(hyperlinks.count)]
            )
            Self.logger.debug("Failed to persist hyperlinks: \(error.localizedDescription, privacy: .public)")
        }
    }

    private func applyUpdatedHyperlinks(_ batch: UpdatedHyperlinksBatch) {
        guard !batch.changes.isEmpty else {
            diagnosticsLogger.log(
                "apply_updated_hyperlinks_skipped",
                details: ["reason": "empty_changes"]
            )
            return
        }

        do {
            let store = try HyperlinkStore.openShared()
            try store.apply(updatedBatch: batch)
        } catch {
            diagnosticsLogger.logError(
                "apply_updated_hyperlinks_failed",
                error: error,
                details: ["change_count": String(batch.changes.count)]
            )
            Self.logger.debug(
                "Failed to apply updated hyperlinks: \(error.localizedDescription, privacy: .public)"
            )
        }
    }

    private func newestUpdatedAt(in hyperlinks: [Hyperlink]) -> String? {
        hyperlinks
            .map(\.updatedAt)
            .max()
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
                hyperlink.summary?.lowercased() ?? "",
                hyperlink.ogDescription?.lowercased() ?? "",
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
        let summary = hyperlink.summary?.lowercased() ?? ""
        let ogDescription = hyperlink.ogDescription?.lowercased() ?? ""
        let url = hyperlink.url.lowercased()
        let rawURL = hyperlink.rawURL.lowercased()
        let host = URL(string: hyperlink.url)?.host?.lowercased() ?? ""

        var score = 0
        for token in tokens {
            if title.contains(token) {
                score += 6
            }
            if summary.contains(token) {
                score += 5
            }
            if ogDescription.contains(token) {
                score += 3
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
