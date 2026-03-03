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
        request.httpBody = buildUploadPDFBody(
            boundary: boundary,
            title: title,
            filename: filename,
            payload: payload
        )
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
        if let authorizationHeaderValue {
            request.setValue(authorizationHeaderValue, forHTTPHeaderField: "Authorization")
        }
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

private struct ShareHyperlinkInput: Encodable {
    let title: String
    let url: String
}

private struct ShareAPIErrorResponse: Decodable {
    let error: String
}
