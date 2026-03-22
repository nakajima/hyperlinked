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
    private let logger = AppEventLogger(component: "APIClient")

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
        logger.log("api_test_connection_started", details: ["server": baseURL.absoluteString])
        let request = try makeRequest(path: "/hyperlinks.json", method: "GET")
        _ = try await send(request)
        logger.log("api_test_connection_succeeded", details: ["server": baseURL.absoluteString])
    }

    func listHyperlinks() async throws -> [Hyperlink] {
        logger.log("api_list_hyperlinks_started", details: ["server": baseURL.absoluteString])
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

        logger.log(
            "api_list_hyperlinks_succeeded",
            details: [
                "server": baseURL.absoluteString,
                "hyperlink_count": String(hyperlinks.count),
                "pages_loaded": String(page + 1),
            ]
        )
        return hyperlinks
    }

    func fetchUpdatedHyperlinks(updatedAt: String) async throws -> UpdatedHyperlinksBatch {
        let normalizedUpdatedAt = updatedAt
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard !normalizedUpdatedAt.isEmpty else {
            logger.log(
                "api_fetch_updated_hyperlinks_rejected",
                details: ["reason": "empty_updated_at"]
            )
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

        logger.log(
            "api_fetch_updated_hyperlinks_succeeded",
            details: [
                "cursor": normalizedUpdatedAt,
                "change_count": String(changes.count),
                "server_updated_at": data.updatedHyperlinks.serverUpdatedAt,
            ]
        )
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
        logger.log(
            "api_create_hyperlink_started",
            details: ["server": baseURL.absoluteString, "url": url]
        )
        let input = HyperlinkInput(title: title, url: url)
        let body = try JSONEncoder().encode(input)
        var request = try makeRequest(path: "/hyperlinks.json", method: "POST")
        request.httpBody = body
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        let data = try await send(request)
        do {
            let created = try JSONDecoder().decode(Hyperlink.self, from: data)
            if let enriched = try? await fetchHyperlink(id: created.id) {
                logger.log(
                    "api_create_hyperlink_succeeded",
                    details: ["hyperlink_id": String(created.id), "enriched": "true"]
                )
                return enriched
            }
            logger.log(
                "api_create_hyperlink_succeeded",
                details: ["hyperlink_id": String(created.id), "enriched": "false"]
            )
            return created
        } catch {
            logger.logError("api_create_hyperlink_decode_failed", error: error)
            throw APIClientError.decodingFailed(error.localizedDescription)
        }
    }

    func uploadPDF(title: String, fileURL: URL, filename: String) async throws -> Hyperlink {
        logger.log(
            "api_upload_pdf_started",
            details: ["server": baseURL.absoluteString, "filename": filename]
        )
        let didStartScopedAccess = fileURL.startAccessingSecurityScopedResource()
        defer {
            if didStartScopedAccess {
                fileURL.stopAccessingSecurityScopedResource()
            }
        }

        let payload = try Data(contentsOf: fileURL)
        let boundary = "Boundary-\(UUID().uuidString)"
        var request = try makeRequest(path: "/uploads", method: "POST")
        request.setValue(
            "multipart/form-data; boundary=\(boundary)",
            forHTTPHeaderField: "Content-Type"
        )
        request.httpBody = buildUploadPDFBody(
            boundary: boundary,
            title: title,
            filename: filename,
            payload: payload
        )
        let data = try await send(request)
        do {
            let created = try JSONDecoder().decode(Hyperlink.self, from: data)
            if let enriched = try? await fetchHyperlink(id: created.id) {
                logger.log(
                    "api_upload_pdf_succeeded",
                    details: ["hyperlink_id": String(created.id), "enriched": "true", "filename": filename]
                )
                return enriched
            }
            logger.log(
                "api_upload_pdf_succeeded",
                details: ["hyperlink_id": String(created.id), "enriched": "false", "filename": filename]
            )
            return created
        } catch {
            logger.logError("api_upload_pdf_decode_failed", error: error, details: ["filename": filename])
            throw APIClientError.decodingFailed(error.localizedDescription)
        }
    }

    func reportHyperlinkClick(hyperlinkID: Int) async throws {
        let request = try makeRequest(
            path: "/hyperlinks/\(hyperlinkID)/click",
            method: "POST"
        )
        _ = try await send(request)
        logger.log("api_report_hyperlink_click_succeeded", details: ["hyperlink_id": String(hyperlinkID)])
    }

    func artifactInlineURL(hyperlinkID: Int, kind: String) -> URL {
        baseURL
            .appendingPathComponent("hyperlinks")
            .appendingPathComponent(String(hyperlinkID))
            .appendingPathComponent("artifacts")
            .appendingPathComponent(kind)
            .appendingPathComponent("inline")
    }

    func fetchArtifactData(hyperlinkID: Int, kind: String) async throws -> Data {
        var request = URLRequest(url: artifactInlineURL(hyperlinkID: hyperlinkID, kind: kind))
        request.httpMethod = "GET"
        request.timeoutInterval = 20
        if let authorizationHeaderValue {
            request.setValue(authorizationHeaderValue, forHTTPHeaderField: "Authorization")
        }
        return try await send(request)
    }

    func fetchArtifactText(hyperlinkID: Int, kind: String) async throws -> String {
        let data = try await fetchArtifactData(hyperlinkID: hyperlinkID, kind: kind)
        guard let string = String(data: data, encoding: .utf8), !string.isEmpty else {
            throw APIClientError.decodingFailed("Artifact \(kind) for hyperlink \(hyperlinkID) was empty.")
        }
        return string
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
        do {
            let (data, response) = try await session.data(for: request)
            guard let http = response as? HTTPURLResponse else {
                logger.log(
                    "api_request_failed",
                    details: [
                        "path": request.url?.path ?? "unknown",
                        "reason": "invalid_response",
                    ]
                )
                throw APIClientError.invalidResponse
            }

            guard (200...299).contains(http.statusCode) else {
                let message = parseErrorMessage(data: data)
                logger.log(
                    "api_request_failed",
                    details: [
                        "path": request.url?.path ?? "unknown",
                        "status_code": String(http.statusCode),
                        "message": message,
                    ]
                )
                throw APIClientError.unexpectedStatus(code: http.statusCode, message: message)
            }

            return data
        } catch {
            logger.logError(
                "api_request_threw",
                error: error,
                details: [
                    "path": request.url?.path ?? "unknown",
                    "method": request.httpMethod ?? "unknown",
                ]
            )
            throw error
        }
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

    private func buildUploadPDFBody(
        boundary: String,
        title: String,
        filename: String,
        payload: Data
    ) -> Data {
        var body = Data()

        appendMultipartField("upload_type", value: "pdf", boundary: boundary, to: &body)
        let trimmedTitle = title.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmedTitle.isEmpty {
            appendMultipartField("title", value: trimmedTitle, boundary: boundary, to: &body)
        }
        appendMultipartField("filename", value: filename, boundary: boundary, to: &body)
        appendMultipartFile(
            fieldName: "file",
            filename: filename,
            mimeType: "application/pdf",
            payload: payload,
            boundary: boundary,
            to: &body
        )
        body.append("--\(boundary)--\r\n".data(using: .utf8) ?? Data())
        return body
    }

    private func appendMultipartField(
        _ name: String,
        value: String,
        boundary: String,
        to body: inout Data
    ) {
        body.append("--\(boundary)\r\n".data(using: .utf8) ?? Data())
        body.append(
            "Content-Disposition: form-data; name=\"\(name)\"\r\n\r\n".data(using: .utf8) ?? Data()
        )
        body.append("\(value)\r\n".data(using: .utf8) ?? Data())
    }

    private func appendMultipartFile(
        fieldName: String,
        filename: String,
        mimeType: String,
        payload: Data,
        boundary: String,
        to body: inout Data
    ) {
        body.append("--\(boundary)\r\n".data(using: .utf8) ?? Data())
        body.append(
            "Content-Disposition: form-data; name=\"\(fieldName)\"; filename=\"\(filename)\"\r\n"
                .data(using: .utf8) ?? Data()
        )
        body.append("Content-Type: \(mimeType)\r\n\r\n".data(using: .utf8) ?? Data())
        body.append(payload)
        body.append("\r\n".data(using: .utf8) ?? Data())
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
