use std::convert::Infallible;

use axum::{
    Json,
    body::Body,
    extract::{FromRef, FromRequestParts},
    http::{HeaderMap, StatusCode, request::Parts},
    response::{IntoResponse, Redirect, Response},
};

use crate::{
    app::controllers::flash::{Flash, FlashName, redirect_with_flash},
    server::{context::Context, views},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RequestFormat {
    Html,
    Json,
}

impl RequestFormat {
    pub(crate) fn from_path(path: &str) -> Self {
        if path.ends_with(".json") {
            Self::Json
        } else {
            Self::Html
        }
    }
}

#[derive(Clone)]
pub(crate) struct ControllerContext {
    state: Context,
    request_headers: HeaderMap,
    format: RequestFormat,
    flash: Flash,
}

impl ControllerContext {
    pub(crate) fn state(&self) -> &Context {
        &self.state
    }

    pub(crate) fn format(&self) -> RequestFormat {
        self.format
    }

    pub(crate) fn page(
        &self,
        title: impl Into<String>,
        body: Result<String, sailfish::RenderError>,
    ) -> ActionResult {
        self.page_with_status(StatusCode::OK, title, body)
    }

    pub(crate) fn page_with_status(
        &self,
        status: StatusCode,
        title: impl Into<String>,
        body: Result<String, sailfish::RenderError>,
    ) -> ActionResult {
        match body {
            Ok(body) => ActionResult::HtmlPage {
                status,
                title: title.into(),
                body,
                flash: self.flash.clone(),
            },
            Err(err) => self.internal_error(format!("failed to render page: {err}")),
        }
    }

    pub(crate) fn json<T: serde::Serialize>(&self, status: StatusCode, payload: T) -> ActionResult {
        match serde_json::to_value(payload) {
            Ok(body) => ActionResult::Json { status, body },
            Err(err) => self.internal_error(format!("failed to serialize json response: {err}")),
        }
    }

    pub(crate) fn temporary_redirect(&self, location: impl Into<String>) -> ActionResult {
        ActionResult::TemporaryRedirect {
            location: location.into(),
        }
    }

    pub(crate) fn redirect_with_flash(
        &self,
        location: impl Into<String>,
        named: FlashName,
        message: impl Into<String>,
    ) -> ActionResult {
        ActionResult::RedirectWithFlash {
            request_headers: self.request_headers.clone(),
            location: location.into(),
            named,
            message: message.into(),
        }
    }

    pub(crate) fn binary(
        &self,
        status: StatusCode,
        headers: HeaderMap,
        body: impl Into<Vec<u8>>,
    ) -> ActionResult {
        ActionResult::Binary {
            status,
            headers,
            body: body.into(),
        }
    }

    pub(crate) fn no_content(&self) -> ActionResult {
        ActionResult::Status(StatusCode::NO_CONTENT)
    }

    pub(crate) fn error(&self, status: StatusCode, message: impl Into<String>) -> ActionResult {
        ActionResult::Error {
            format: self.format,
            status,
            message: message.into(),
        }
    }

    pub(crate) fn bad_request(&self, message: impl Into<String>) -> ActionResult {
        self.error(StatusCode::BAD_REQUEST, message)
    }

    pub(crate) fn internal_error(&self, message: impl Into<String>) -> ActionResult {
        self.error(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
}

impl<S> FromRequestParts<S> for ControllerContext
where
    Context: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let request_headers = parts.headers.clone();
        let format = RequestFormat::from_path(parts.uri.path());
        let flash = Flash::from_headers(&request_headers);

        Ok(Self {
            state: Context::from_ref(state),
            request_headers,
            format,
            flash,
        })
    }
}

pub(crate) enum ActionResult {
    HtmlPage {
        status: StatusCode,
        title: String,
        body: String,
        flash: Flash,
    },
    Json {
        status: StatusCode,
        body: serde_json::Value,
    },
    TemporaryRedirect {
        location: String,
    },
    RedirectWithFlash {
        request_headers: HeaderMap,
        location: String,
        named: FlashName,
        message: String,
    },
    Binary {
        status: StatusCode,
        headers: HeaderMap,
        body: Vec<u8>,
    },
    Error {
        format: RequestFormat,
        status: StatusCode,
        message: String,
    },
    Status(StatusCode),
}

impl IntoResponse for ActionResult {
    fn into_response(self) -> Response {
        match self {
            ActionResult::HtmlPage {
                status,
                title,
                body,
                flash,
            } => {
                let mut response =
                    views::render_html_page_with_flash(title.as_str(), Ok(body), flash);
                *response.status_mut() = status;
                response
            }
            ActionResult::Json { status, body } => (status, Json(body)).into_response(),
            ActionResult::TemporaryRedirect { location } => {
                Redirect::temporary(location.as_str()).into_response()
            }
            ActionResult::RedirectWithFlash {
                request_headers,
                location,
                named,
                message,
            } => redirect_with_flash(&request_headers, location.as_str(), named, message),
            ActionResult::Binary {
                status,
                headers,
                body,
            } => {
                let mut response = Response::new(Body::from(body));
                *response.status_mut() = status;
                response.headers_mut().extend(headers);
                response
            }
            ActionResult::Error {
                format,
                status,
                message,
            } => match format {
                RequestFormat::Html => {
                    views::render_error_page(status, message, "/hyperlinks", "Back to hyperlinks")
                }
                RequestFormat::Json => {
                    (status, Json(serde_json::json!({ "error": message }))).into_response()
                }
            },
            ActionResult::Status(status) => status.into_response(),
        }
    }
}
