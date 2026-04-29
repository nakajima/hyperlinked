import Foundation

enum ShareAPIClientError: LocalizedError {
    case invalidURL
    case invalidResponse
    case unexpectedStatus(code: Int, message: String)

    var errorDescription: String? {
        switch self {
        case .invalidURL:
            return "The configured server URL is invalid."
        case .invalidResponse:
            return "The server response was invalid."
        case .unexpectedStatus(let code, let message):
            if message.isEmpty {
                return "Server returned HTTP \(code)."
            }
            return "Server returned HTTP \(code): \(message)"
        }
    }
}

struct ShareAPIClient {
    let baseURL: URL
    let session: URLSession
    let authorizationHeaderValue: String?

    init(
        baseURL: URL,
        authorizationHeaderValue: String? = nil,
        session: URLSession = .shared
    ) {
        self.baseURL = baseURL
        self.session = session
        self.authorizationHeaderValue = authorizationHeaderValue
    }

    func createHyperlink(title: String, url: String) async throws {
        var request = try makeRequest(path: "/hyperlinks.json", method: "POST")
        request.httpBody = try JSONEncoder().encode(ShareHyperlinkInput(title: title, url: url))
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        _ = try await send(request)
    }

    func uploadPDF(title: String, fileURL: URL, filename: String) async throws {
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
        request.httpBody = HTTPRequestBuilder.buildPDFUploadBody(
            boundary: boundary,
            title: title,
            filename: filename,
            payload: payload
        )
        _ = try await send(request)
    }

    private func makeRequest(path: String, method: String) throws -> URLRequest {
        do {
            return try HTTPRequestBuilder.makeRequest(
                baseURL: baseURL,
                path: path,
                method: method,
                authorizationHeaderValue: authorizationHeaderValue
            )
        } catch let error as HTTPRequestBuilderError {
            throw map(error)
        }
    }

    private func send(_ request: URLRequest) async throws -> Data {
        do {
            return try await HTTPRequestBuilder.send(request, session: session)
        } catch let error as HTTPRequestBuilderError {
            throw map(error)
        }
    }

    private func map(_ error: HTTPRequestBuilderError) -> ShareAPIClientError {
        switch error {
        case .invalidURL:
            return .invalidURL
        case .invalidResponse:
            return .invalidResponse
        case .unexpectedStatus(let code, let message):
            return .unexpectedStatus(code: code, message: message)
        }
    }
}

private struct ShareHyperlinkInput: Encodable {
    let title: String
    let url: String
}

