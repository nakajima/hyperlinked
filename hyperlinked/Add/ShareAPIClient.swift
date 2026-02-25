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

    init(baseURL: URL, session: URLSession = .shared) {
        self.baseURL = baseURL
        self.session = session
    }

    func createHyperlink(title: String, url: String) async throws {
        var request = try makeRequest(path: "/hyperlinks.json", method: "POST")
        request.httpBody = try JSONEncoder().encode(ShareHyperlinkInput(title: title, url: url))
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        _ = try await send(request)
    }

    private func makeRequest(path: String, method: String) throws -> URLRequest {
        let cleanPath = path.hasPrefix("/") ? String(path.dropFirst()) : path
        let endpoint = baseURL.appendingPathComponent(cleanPath)
        guard let scheme = endpoint.scheme, (scheme == "http" || scheme == "https") else {
            throw ShareAPIClientError.invalidURL
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
            throw ShareAPIClientError.invalidResponse
        }

        guard (200...299).contains(http.statusCode) else {
            let message = parseErrorMessage(from: data)
            throw ShareAPIClientError.unexpectedStatus(code: http.statusCode, message: message)
        }
        return data
    }

    private func parseErrorMessage(from data: Data) -> String {
        if let parsed = try? JSONDecoder().decode(ShareAPIErrorResponse.self, from: data) {
            return parsed.error
        }
        if let raw = String(data: data, encoding: .utf8) {
            return raw.trimmingCharacters(in: .whitespacesAndNewlines)
        }
        return ""
    }
}

private struct ShareHyperlinkInput: Encodable {
    let title: String
    let url: String
}

private struct ShareAPIErrorResponse: Decodable {
    let error: String
}
