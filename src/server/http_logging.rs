use std::time::Instant;

use axum::{
    body::{Body, to_bytes},
    extract::Request,
    http::{Method, header},
    middleware::Next,
    response::Response,
};

const MAX_LOGGED_BODY_BYTES: usize = 64 * 1024;
const MAX_LOGGED_BODY_CHARS: usize = 4 * 1024;

pub async fn log_requests(req: Request, next: Next) -> Response {
    let started_at = Instant::now();
    let (parts, body) = req.into_parts();

    let method = parts.method.clone();
    let uri = parts.uri.clone();
    let content_type = content_type_header(parts.headers.get(header::CONTENT_TYPE));
    let content_length = content_length_header(parts.headers.get(header::CONTENT_LENGTH));
    let query = summarize_query(uri.query());

    let (request, body_summary) = if should_log_body(&method, content_type.as_deref(), content_length) {
        match to_bytes(body, MAX_LOGGED_BODY_BYTES).await {
            Ok(bytes) => {
                let summary = summarize_body(content_type.as_deref(), &bytes);
                let req = Request::from_parts(parts, Body::from(bytes));
                (req, summary)
            }
            Err(err) => {
                let req = Request::from_parts(parts, Body::empty());
                (req, format!("<failed to read body: {err}>"))
            }
        }
    } else {
        let req = Request::from_parts(parts, body);
        (req, "<not logged>".to_string())
    };

    let response = next.run(request).await;
    let latency_ms = started_at.elapsed().as_millis();

    tracing::info!(
        method = %method,
        uri = %uri,
        query = %query,
        request_content_type = %content_type.as_deref().unwrap_or(""),
        request_body = %body_summary,
        status = response.status().as_u16(),
        latency_ms,
        "http request"
    );

    response
}

fn content_type_header(value: Option<&header::HeaderValue>) -> Option<String> {
    value.and_then(|header_value| header_value.to_str().ok().map(str::to_string))
}

fn content_length_header(value: Option<&header::HeaderValue>) -> Option<usize> {
    value
        .and_then(|header_value| header_value.to_str().ok())
        .and_then(|raw| raw.parse::<usize>().ok())
}

fn summarize_query(query: Option<&str>) -> String {
    let Some(raw_query) = query else {
        return String::new();
    };

    match serde_urlencoded::from_str::<Vec<(String, String)>>(raw_query) {
        Ok(params) => params
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&"),
        Err(_) => raw_query.to_string(),
    }
}

fn should_log_body(method: &Method, content_type: Option<&str>, content_length: Option<usize>) -> bool {
    if method == Method::GET || method == Method::HEAD {
        return false;
    }

    let Some(content_type) = content_type else {
        return false;
    };
    let Some(content_length) = content_length else {
        return false;
    };
    if content_length > MAX_LOGGED_BODY_BYTES {
        return false;
    }

    let normalized = content_type.to_ascii_lowercase();
    normalized.starts_with("application/json")
        || normalized.starts_with("application/x-www-form-urlencoded")
        || normalized.starts_with("text/")
}

fn summarize_body(content_type: Option<&str>, body: &[u8]) -> String {
    let mut decoded = String::from_utf8_lossy(body).to_string();
    if decoded.len() > MAX_LOGGED_BODY_CHARS {
        decoded.truncate(MAX_LOGGED_BODY_CHARS);
        decoded.push_str("...<truncated>");
    }

    if content_type
        .map(|value| value.starts_with("application/x-www-form-urlencoded"))
        .unwrap_or(false)
    {
        match serde_urlencoded::from_bytes::<Vec<(String, String)>>(decoded.as_bytes()) {
            Ok(parsed) => {
                return parsed
                    .into_iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("&");
            }
            Err(_) => return decoded,
        }
    }

    decoded
}
