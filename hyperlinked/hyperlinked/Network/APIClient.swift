import Apollo
import ApolloAPI
import Foundation

enum APIClientError: LocalizedError {
    case invalidURL
    case invalidResponse
    case unexpectedStatus(code: Int, message: String)
    case decodingFailed(String)
    case graphqlError(String)

    var errorDescription: String? {
        switch self {
        case .invalidURL:
            return "The configured server URL is invalid."
        case .invalidResponse:
            return "The server response was invalid."
        case .unexpectedStatus(let code, let message):
            if message.isEmpty {
                return "The server returned HTTP \(code)."
            }
            return "The server returned HTTP \(code): \(message)"
        case .decodingFailed(let message):
            return "Failed to decode server response: \(message)"
        case .graphqlError(let message):
            return "GraphQL error: \(message)"
        }
    }
}

struct APIClient {
    let baseURL: URL
    let session: URLSession
    let authorizationHeaderValue: String?

    private let apollo: ApolloClient

    init(
        baseURL: URL,
        authorizationHeaderValue: String? = nil,
        session: URLSession = .shared
    ) {
        self.baseURL = baseURL
        self.session = session
        self.authorizationHeaderValue = authorizationHeaderValue
        let store = ApolloStore(cache: InMemoryNormalizedCache())
        let sessionClient = URLSessionClient(
            sessionConfiguration: session.configuration,
            callbackQueue: nil
        )
        let interceptorProvider = DefaultInterceptorProvider(
            client: sessionClient,
            shouldInvalidateClientOnDeinit: true,
            store: store
        )
        var additionalHeaders: [String: String] = [:]
        if let authorizationHeaderValue {
            additionalHeaders["Authorization"] = authorizationHeaderValue
        }
        let transport = RequestChainNetworkTransport(
            interceptorProvider: interceptorProvider,
            endpointURL: baseURL.appendingPathComponent("graphql"),
            additionalHeaders: additionalHeaders
        )
        self.apollo = ApolloClient(networkTransport: transport, store: store)
    }

    func testConnection() async throws {
        let request = try makeRequest(path: "/hyperlinks.json", method: "GET")
        _ = try await send(request)
    }

    func listHyperlinks() async throws -> [Hyperlink] {
        let pageSize = 200
        let maxPages = 50
        var page = 0
        var hyperlinks: [Hyperlink] = []
        var seenIDs = Set<Int>()

        while page < maxPages {
            let data = try await executeQuery(
                HyperlinkedAPI.HyperlinksIndexPageQuery(limit: pageSize, page: page)
            )
            let batch = data.hyperlinks.nodes.map {
                toHyperlink(fields: $0.fragments.hyperlinkFields)
            }
            if batch.isEmpty {
                break
            }

            for hyperlink in batch {
                if seenIDs.insert(hyperlink.id).inserted {
                    hyperlinks.append(hyperlink)
                }
            }

            if batch.count < pageSize {
                break
            }
            page += 1
        }

        return hyperlinks
    }

    func fetchUpdatedHyperlinks(updatedAt: String) async throws -> UpdatedHyperlinksBatch {
        let normalizedUpdatedAt = updatedAt
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard !normalizedUpdatedAt.isEmpty else {
            throw APIClientError.decodingFailed("updatedAt must not be empty.")
        }

        let data = try await executeQuery(HyperlinkedAPI.UpdatedHyperlinksQuery(updatedAt: normalizedUpdatedAt))

        let changes = try data.updatedHyperlinks.changes.map { change in
            UpdatedHyperlinkChange(
                id: change.id,
                changeType: try toChangeType(change.changeType),
                updatedAt: change.updatedAt,
                hyperlink: change.hyperlink.map { toHyperlink(fields: $0.fragments.hyperlinkFields) }
            )
        }

        return UpdatedHyperlinksBatch(
            serverUpdatedAt: data.updatedHyperlinks.serverUpdatedAt,
            changes: changes
        )
    }

    func fetchHyperlink(id: Int) async throws -> Hyperlink {
        let data = try await executeQuery(HyperlinkedAPI.HyperlinkDetailQuery(id: id))
        guard let first = data.hyperlinks.nodes.first else {
            throw APIClientError.unexpectedStatus(
                code: 404,
                message: "hyperlink \(id) not found"
            )
        }
        return toHyperlink(fields: first.fragments.hyperlinkFields)
    }

    func createHyperlink(title: String, url: String) async throws -> Hyperlink {
        let input = HyperlinkInput(title: title, url: url)
        let body = try JSONEncoder().encode(input)
        var request = try makeRequest(path: "/hyperlinks.json", method: "POST")
        request.httpBody = body
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        let data = try await send(request)
        do {
            let created = try JSONDecoder().decode(Hyperlink.self, from: data)
            if let enriched = try? await fetchHyperlink(id: created.id) {
                return enriched
            }
            return created
        } catch {
            throw APIClientError.decodingFailed(error.localizedDescription)
        }
    }

    func reportHyperlinkClick(hyperlinkID: Int) async throws {
        let request = try makeRequest(
            path: "/hyperlinks/\(hyperlinkID)/click",
            method: "POST"
        )
        _ = try await send(request)
    }

    func artifactInlineURL(hyperlinkID: Int, kind: String) -> URL {
        baseURL
            .appendingPathComponent("hyperlinks")
            .appendingPathComponent(String(hyperlinkID))
            .appendingPathComponent("artifacts")
            .appendingPathComponent(kind)
            .appendingPathComponent("inline")
    }

    private func executeQuery<Query: GraphQLQuery>(_ query: Query) async throws -> Query.Data {
        try await withCheckedThrowingContinuation { continuation in
            _ = apollo.fetch(
                query: query,
                cachePolicy: .fetchIgnoringCacheCompletely,
                queue: .global(qos: .userInitiated)
            ) { result in
                switch result {
                case .success(let graphQLResult):
                    if let errors = graphQLResult.errors, !errors.isEmpty {
                        let message = errors
                            .compactMap(\.message)
                            .joined(separator: "\n")
                        continuation.resume(
                            throwing: APIClientError.graphqlError(
                                message.isEmpty ? "Unknown GraphQL error" : message
                            )
                        )
                        return
                    }

                    guard let data = graphQLResult.data else {
                        continuation.resume(
                            throwing: APIClientError.decodingFailed(
                                "GraphQL payload is missing `data`."
                            )
                        )
                        return
                    }

                    continuation.resume(returning: data)
                case .failure(let error):
                    continuation.resume(
                        throwing: APIClientError.decodingFailed(error.localizedDescription)
                    )
                }
            }
        }
    }

    private func toChangeType(
        _ value: GraphQLEnum<HyperlinkedAPI.HyperlinkChangeType>
    ) throws -> UpdatedHyperlinkChange.ChangeType {
        switch value.value {
        case .updated:
            return .updated
        case .deleted:
            return .deleted
        case nil:
            throw APIClientError.decodingFailed("Unknown changeType: \(value.rawValue)")
        }
    }

    private func toHyperlink(fields: HyperlinkedAPI.HyperlinkFields) -> Hyperlink {
        Hyperlink(
            id: fields.id,
            title: fields.title,
            url: fields.url,
            rawURL: fields.rawUrl,
            ogDescription: fields.ogDescription,
            isURLValid: nil,
            discoveryDepth: fields.discoveryDepth,
            clicksCount: fields.clicksCount,
            lastClickedAt: fields.lastClickedAt,
            processingState: "ready",
            createdAt: fields.createdAt,
            updatedAt: fields.updatedAt,
            thumbnailURL: fields.thumbnailUrl,
            thumbnailDarkURL: fields.thumbnailDarkUrl,
            screenshotURL: fields.screenshotUrl,
            screenshotDarkURL: fields.screenshotDarkUrl,
            discoveredVia: fields.discoveredVia.map(toHyperlinkRef)
        )
    }

    private func toHyperlinkRef(_ model: HyperlinkedAPI.HyperlinkFields.DiscoveredVium) -> HyperlinkRef {
        HyperlinkRef(
            id: model.id,
            title: model.title,
            url: model.url,
            rawURL: model.rawUrl
        )
    }

    private func makeRequest(path: String, method: String) throws -> URLRequest {
        let cleanPath = path.hasPrefix("/") ? String(path.dropFirst()) : path
        let endpoint = baseURL.appendingPathComponent(cleanPath)
        guard let scheme = endpoint.scheme, (scheme == "http" || scheme == "https") else {
            throw APIClientError.invalidURL
        }

        var request = URLRequest(url: endpoint)
        request.httpMethod = method
        request.timeoutInterval = 15
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if let authorizationHeaderValue {
            request.setValue(authorizationHeaderValue, forHTTPHeaderField: "Authorization")
        }
        return request
    }

    private func send(_ request: URLRequest) async throws -> Data {
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else {
            throw APIClientError.invalidResponse
        }

        guard (200...299).contains(http.statusCode) else {
            let message = parseErrorMessage(data: data)
            throw APIClientError.unexpectedStatus(code: http.statusCode, message: message)
        }

        return data
    }

    private func parseErrorMessage(data: Data) -> String {
        if let parsed = try? JSONDecoder().decode(APIErrorResponse.self, from: data) {
            return parsed.error
        }

        if let raw = String(data: data, encoding: .utf8) {
            return raw.trimmingCharacters(in: .whitespacesAndNewlines)
        }

        return ""
    }
}

struct UpdatedHyperlinksBatch {
    let serverUpdatedAt: String
    let changes: [UpdatedHyperlinkChange]
}

struct UpdatedHyperlinkChange {
    enum ChangeType: String, Decodable {
        case updated = "UPDATED"
        case deleted = "DELETED"
    }

    let id: Int
    let changeType: ChangeType
    let updatedAt: String
    let hyperlink: Hyperlink?
}
