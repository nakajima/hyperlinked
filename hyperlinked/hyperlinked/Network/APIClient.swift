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

    init(baseURL: URL, session: URLSession = .shared) {
        self.baseURL = baseURL
        self.session = session
    }

    func testConnection() async throws {
        _ = try await listHyperlinks()
    }

    func listHyperlinks(q: String? = nil) async throws -> [Hyperlink] {
        let normalizedQ = q?
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .nilIfEmpty
        let payload: GraphQLHyperlinksPayload = try await sendGraphQL(
            query: Self.hyperlinksQuery,
            variables: normalizedQ.map { ["q": $0] }
        )
        return payload.hyperlinks.nodes.map { $0.toHyperlink() }
    }

    func fetchHyperlink(id: Int) async throws -> Hyperlink {
        let payload: GraphQLHyperlinksPayload = try await sendGraphQL(
            query: Self.hyperlinkByIDQuery(id: id)
        )
        guard let first = payload.hyperlinks.nodes.first else {
            throw APIClientError.unexpectedStatus(
                code: 404,
                message: "hyperlink \(id) not found"
            )
        }
        return first.toHyperlink()
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

    func artifactInlineURL(hyperlinkID: Int, kind: String) -> URL {
        baseURL
            .appendingPathComponent("hyperlinks")
            .appendingPathComponent(String(hyperlinkID))
            .appendingPathComponent("artifacts")
            .appendingPathComponent(kind)
            .appendingPathComponent("inline")
    }

    private func sendGraphQL<T: Decodable>(
        query: String,
        variables: [String: String]? = nil
    ) async throws -> T {
        var request = try makeRequest(path: "/graphql", method: "POST")
        request.httpBody = try JSONEncoder().encode(
            GraphQLRequestPayload(query: query, variables: variables)
        )
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        let data = try await send(request)

        do {
            let decoded = try JSONDecoder().decode(GraphQLResponsePayload<T>.self, from: data)
            if let message = decoded.errors?.first?.message, !message.isEmpty {
                throw APIClientError.graphqlError(message)
            }
            guard let payload = decoded.data else {
                throw APIClientError.decodingFailed("GraphQL payload is missing `data`.")
            }
            return payload
        } catch let error as APIClientError {
            throw error
        } catch {
            throw APIClientError.decodingFailed(error.localizedDescription)
        }
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

    private static let hyperlinkFields = """
      id
      title
      url
      rawUrl
      clicksCount
      lastClickedAt
      createdAt
      updatedAt
      thumbnailUrl
      thumbnailDarkUrl
      screenshotUrl
      screenshotDarkUrl
      hyperlinkProcessingJob(
        pagination: { page: { limit: 1, page: 0 } }
        orderBy: { id: DESC }
      ) {
        nodes { state }
      }
    """

    private static let hyperlinksQuery = """
    query HyperlinksIndex($q: String) {
      hyperlinks(
        q: $q
        pagination: { page: { limit: 200, page: 0 } }
        orderBy: { id: DESC }
      ) {
        nodes {
    \(hyperlinkFields)
        }
      }
    }
    """

    private static func hyperlinkByIDQuery(id: Int) -> String {
        """
        query HyperlinkDetail {
          hyperlinks(
            filters: { id: { eq: \(id) } }
            pagination: { page: { limit: 1, page: 0 } }
          ) {
            nodes {
        \(hyperlinkFields)
            }
          }
        }
        """
    }
}

private struct GraphQLRequestPayload: Encodable {
    let query: String
    let variables: [String: String]?
}

private struct GraphQLResponsePayload<T: Decodable>: Decodable {
    let data: T?
    let errors: [GraphQLErrorPayload]?
}

private struct GraphQLErrorPayload: Decodable {
    let message: String
}

private struct GraphQLHyperlinksPayload: Decodable {
    let hyperlinks: GraphQLHyperlinksConnectionPayload
}

private struct GraphQLHyperlinksConnectionPayload: Decodable {
    let nodes: [GraphQLHyperlinkNodePayload]
}

private struct GraphQLHyperlinkNodePayload: Decodable {
    let id: Int
    let title: String
    let url: String
    let rawUrl: String
    let clicksCount: Int
    let lastClickedAt: String?
    let createdAt: String
    let updatedAt: String
    let thumbnailUrl: String?
    let thumbnailDarkUrl: String?
    let screenshotUrl: String?
    let screenshotDarkUrl: String?
    let hyperlinkProcessingJob: GraphQLProcessingJobConnectionPayload?

    func toHyperlink() -> Hyperlink {
        Hyperlink(
            id: id,
            title: title,
            url: url,
            rawURL: rawUrl,
            clicksCount: clicksCount,
            lastClickedAt: lastClickedAt,
            processingState: hyperlinkProcessingJob?.nodes.first?.state ?? "idle",
            createdAt: createdAt,
            updatedAt: updatedAt,
            thumbnailURL: thumbnailUrl,
            thumbnailDarkURL: thumbnailDarkUrl,
            screenshotURL: screenshotUrl,
            screenshotDarkURL: screenshotDarkUrl
        )
    }
}

private struct GraphQLProcessingJobConnectionPayload: Decodable {
    let nodes: [GraphQLProcessingJobPayload]
}

private struct GraphQLProcessingJobPayload: Decodable {
    let state: String
}

private extension String {
    var nilIfEmpty: String? {
        isEmpty ? nil : self
    }
}
