use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};
use sailfish::{RenderError, TemplateSimple};

use super::html_layout;

#[derive(TemplateSimple)]
#[template(path = "errors/simple.stpl")]
struct ErrorBodyTemplate<'a> {
    status_code: u16,
    status_text: &'a str,
    message: &'a str,
    back_href: &'a str,
    back_label: &'a str,
}

pub(crate) fn render_html_page(title: &str, body: Result<String, RenderError>) -> Response {
    render_html_page_with_status(StatusCode::OK, title, body)
}

pub(crate) fn render_html_page_with_status(
    status: StatusCode,
    title: &str,
    body: Result<String, RenderError>,
) -> Response {
    let body = match body {
        Ok(body) => body,
        Err(err) => return template_render_failure_response(err),
    };

    match html_layout::page(title, &body) {
        Ok(html) => (status, html).into_response(),
        Err(err) => template_render_failure_response(err),
    }
}

pub(crate) fn render_error_page(
    status: StatusCode,
    message: impl Into<String>,
    back_href: &str,
    back_label: &str,
) -> Response {
    let message = message.into();
    let body = ErrorBodyTemplate {
        status_code: status.as_u16(),
        status_text: status.canonical_reason().unwrap_or("Error"),
        message: &message,
        back_href,
        back_label,
    }
    .render_once();

    render_html_page_with_status(status, "Error", body)
}

fn template_render_failure_response(err: RenderError) -> Response {
    tracing::error!(error = ?err, "failed to render HTML template");

    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Html(
            "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Internal Server Error</title></head><body><h2>500 Internal Server Error</h2><p>Failed to render template.</p></body></html>".to_string(),
        ),
    )
        .into_response()
}
