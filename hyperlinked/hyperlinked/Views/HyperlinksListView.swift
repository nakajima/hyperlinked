import SwiftUI

struct HyperlinksListView: View {
    @EnvironmentObject private var appModel: AppModel
    @Environment(\.colorScheme) private var colorScheme

    @State private var hyperlinks: [Hyperlink] = []
    @State private var isLoading = false
    @State private var errorMessage: String?
    @State private var activeSheet: ActiveSheet?
    @State private var isSearchPresented = false
    @State private var queryText = ""
    @AppStorage("hyperlinks.view_options.show_discovered_links")
    private var showDiscoveredLinks = false
    @AppStorage("hyperlinks.view_options.order_override")
    private var orderOverrideRawValue = ""
    @State private var pendingFilterTask: Task<Void, Never>?

    private enum ActiveSheet: String, Identifiable {
        case add
        case settings

        var id: String { rawValue }
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
            return [.newest, .relevance, .oldest, .mostClicked, .recentlyClicked, .random]
        }
        return [.newest, .oldest, .mostClicked, .recentlyClicked, .random]
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

    private var queryString: String? {
        var tokens: [String] = []
        if hasFreeText {
            tokens.append(trimmedQueryText)
        }
        if let orderOverride {
            tokens.append("order:\(orderOverride.rawValue)")
        }
        if showDiscoveredLinks {
            tokens.append("with:discovered")
        }
        let query = tokens.joined(separator: " ")
        return query.isEmpty ? nil : query
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
                prompt: "Search for something. Or enter a link to add."
            )
        }
        .task(id: appModel.selectedServerURL?.absoluteString) {
            await loadHyperlinks()
        }
        .refreshable {
            await loadHyperlinks()
        }
        .onSubmit(of: .search) {
            Task {
                await loadHyperlinks()
            }
        }
        .onChange(of: queryText) { _ in
            if !hasFreeText, orderOverride == .relevance {
                orderOverrideRawValue = ""
            }
            scheduleFilterReload()
        }
        .onChange(of: showDiscoveredLinks) { _ in
            scheduleFilterReload()
        }
        .onChange(of: orderOverrideRawValue) { _ in
            scheduleFilterReload()
        }
        .sheet(item: $activeSheet) { sheet in
            switch sheet {
            case .add:
                AddHyperlinkView { created in
                    hyperlinks.removeAll { $0.id == created.id }
                    hyperlinks.insert(created, at: 0)
                }
                .environmentObject(appModel)
            case .settings:
                ServerSettingsView {
                    activeSheet = nil
                    appModel.openServerSetup()
                }
                .environmentObject(appModel)
            }
        }
    }

    private var listContent: some View {
        List {
            if isLoading && hyperlinks.isEmpty {
                Section {
                    HStack {
                        Spacer()
                        ProgressView("Loading hyperlinks…")
                        Spacer()
                    }
                    .padding(.vertical, 24)
                    .listRowSeparator(.hidden)
                }
            } else if let errorMessage, hyperlinks.isEmpty {
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
            } else if hyperlinks.isEmpty {
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
                    ForEach(hyperlinks) { hyperlink in
                        NavigationLink(
                            destination: HyperlinkDetailView(hyperlinkID: hyperlink.id, fallback: hyperlink)
                        ) {
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
                                    HStack(spacing: 12) {
                                        Text(hyperlink.processingState.capitalized)
                                        Text("\(hyperlink.clicksCount) clicks")
                                    }
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                }
                            }
                            .padding(.vertical, 4)
                        }
                    }
                }
            }
        }
        .listStyle(.plain)
    }

    private func scheduleFilterReload() {
        pendingFilterTask?.cancel()
        pendingFilterTask = Task {
            try? await Task.sleep(nanoseconds: 120_000_000)
            guard !Task.isCancelled else {
                return
            }
            pendingFilterTask = nil
            await loadHyperlinks()
        }
    }

    private func loadHyperlinks() async {
        pendingFilterTask?.cancel()

        guard let client = appModel.apiClient else {
            hyperlinks = []
            errorMessage = "No server selected."
            return
        }

        isLoading = true
        defer { isLoading = false }

        do {
            hyperlinks = try await client.listHyperlinks(q: queryString)
            errorMessage = nil
        } catch is CancellationError {
            return
        } catch let urlError as URLError where urlError.code == .cancelled {
            return
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}

private enum HyperlinkOrderFilter: String, Identifiable {
    case newest
    case relevance
    case oldest
    case mostClicked = "most-clicked"
    case recentlyClicked = "recently-clicked"
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
        case .random:
            return "Random"
        }
    }
}
