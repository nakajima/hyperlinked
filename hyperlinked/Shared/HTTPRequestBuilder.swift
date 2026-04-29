import Foundation

enum HTTPRequestBuilderError: LocalizedError {
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
                return "The server returned HTTP \(code)."
            }
            return "The server returned HTTP \(code): \(message)"
        }
    }
}

enum HTTPRequestBuilder {
    static func makeRequest(
        baseURL: URL,
        path: String,
        method: String,
        authorizationHeaderValue: String? = nil,
        timeout: TimeInterval = 15
    ) throws -> URLRequest {
        let cleanPath = path.hasPrefix("/") ? String(path.dropFirst()) : path
        let endpoint = baseURL.appendingPathComponent(cleanPath)
        guard let scheme = endpoint.scheme, (scheme == "http" || scheme == "https") else {
            throw HTTPRequestBuilderError.invalidURL
        }

        var request = URLRequest(url: endpoint)
        request.httpMethod = method
        request.timeoutInterval = timeout
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if let authorizationHeaderValue {
            request.setValue(authorizationHeaderValue, forHTTPHeaderField: "Authorization")
        }
        return request
    }

    static func send(
        _ request: URLRequest,
        session: URLSession,
        parseErrorMessage: (Data) -> String = parseErrorMessage(from:)
    ) async throws -> Data {
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else {
            throw HTTPRequestBuilderError.invalidResponse
        }

        guard (200...299).contains(http.statusCode) else {
            let message = parseErrorMessage(data)
            throw HTTPRequestBuilderError.unexpectedStatus(code: http.statusCode, message: message)
        }
        return data
    }

    static func parseErrorMessage(from data: Data) -> String {
        if let parsed = try? JSONDecoder().decode(ErrorMessageResponse.self, from: data) {
            return parsed.error
        }
        if let raw = String(data: data, encoding: .utf8) {
            return raw.trimmingCharacters(in: .whitespacesAndNewlines)
        }
        return ""
    }

    static func buildPDFUploadBody(
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

    private static func appendMultipartField(
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

    private static func appendMultipartFile(
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

private struct ErrorMessageResponse: Decodable {
    let error: String
}
