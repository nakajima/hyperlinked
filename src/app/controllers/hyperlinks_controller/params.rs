use std::collections::HashMap;

use axum::{
    RequestExt, body,
    body::Body,
    extract::{Path, Request},
    http::{Method, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};

use super::result::{ActionResult, RequestFormat};

pub(crate) struct ParamsRejection(pub(crate) ActionResult);

impl IntoResponse for ParamsRejection {
    fn into_response(self) -> Response {
        self.0.into_response()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct HyperlinkPathId {
    id: i32,
}

impl HyperlinkPathId {
    pub(crate) fn get(self) -> i32 {
        self.id
    }
}

impl<'de> Deserialize<'de> for HyperlinkPathId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        let trimmed = raw.strip_suffix(".json").unwrap_or(raw.as_str());
        let id = trimmed.parse::<i32>().map_err(serde::de::Error::custom)?;
        Ok(Self { id })
    }
}

pub(crate) async fn extract<T, S>(mut request: Request, _state: &S) -> Result<T, ParamsRejection>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    let format = RequestFormat::from_path(request.uri().path());

    let path_params = request
        .extract_parts::<Option<Path<HashMap<String, String>>>>()
        .await
        .map(|params| params.map(|Path(params)| params).unwrap_or_default())
        .map_err(|err| {
            ParamsRejection(ActionResult::Error {
                format,
                status: axum::http::StatusCode::BAD_REQUEST,
                message: format!("failed to parse route params: {err}"),
            })
        })?;

    let query_params = parse_urlencoded_map(request.uri().query().unwrap_or_default(), format)?;
    let body_params = extract_body_params(&mut request, format).await?;

    let mut merged = query_params;
    merged.extend(body_params);
    merged.extend(path_params);

    let encoded = serde_urlencoded::to_string(&merged).map_err(|err| {
        ParamsRejection(ActionResult::Error {
            format,
            status: axum::http::StatusCode::BAD_REQUEST,
            message: format!("failed to encode params: {err}"),
        })
    })?;

    serde_urlencoded::from_str::<T>(&encoded).map_err(|err| {
        ParamsRejection(ActionResult::Error {
            format,
            status: axum::http::StatusCode::BAD_REQUEST,
            message: format!("invalid params: {err}"),
        })
    })
}

fn parse_urlencoded_map(
    source: &str,
    format: RequestFormat,
) -> Result<HashMap<String, String>, ParamsRejection> {
    if source.is_empty() {
        return Ok(HashMap::new());
    }

    serde_urlencoded::from_str::<HashMap<String, String>>(source).map_err(|err| {
        ParamsRejection(ActionResult::Error {
            format,
            status: axum::http::StatusCode::BAD_REQUEST,
            message: format!("failed to parse query params: {err}"),
        })
    })
}

async fn extract_body_params(
    request: &mut Request,
    format: RequestFormat,
) -> Result<HashMap<String, String>, ParamsRejection> {
    if !matches!(
        *request.method(),
        Method::POST | Method::PATCH | Method::PUT | Method::DELETE
    ) {
        return Ok(HashMap::new());
    }

    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let request_body = std::mem::replace(request.body_mut(), Body::empty());
    let body = body::to_bytes(request_body, usize::MAX)
        .await
        .map_err(|err| {
            ParamsRejection(ActionResult::Error {
                format,
                status: axum::http::StatusCode::BAD_REQUEST,
                message: format!("failed to read request body: {err}"),
            })
        })?;

    if body.is_empty() {
        return Ok(HashMap::new());
    }

    let Some(content_type) = content_type else {
        return Err(ParamsRejection(ActionResult::Error {
            format,
            status: axum::http::StatusCode::BAD_REQUEST,
            message: "missing content-type for request body".to_string(),
        }));
    };

    if content_type.starts_with("application/x-www-form-urlencoded") {
        return serde_urlencoded::from_bytes::<HashMap<String, String>>(&body).map_err(|err| {
            ParamsRejection(ActionResult::Error {
                format,
                status: axum::http::StatusCode::BAD_REQUEST,
                message: format!("failed to parse form body: {err}"),
            })
        });
    }

    if content_type.starts_with("application/json") {
        let value = serde_json::from_slice::<serde_json::Value>(&body).map_err(|err| {
            ParamsRejection(ActionResult::Error {
                format,
                status: axum::http::StatusCode::BAD_REQUEST,
                message: format!("failed to parse json body: {err}"),
            })
        })?;

        return json_object_to_string_map(value).map_err(|message| {
            ParamsRejection(ActionResult::Error {
                format,
                status: axum::http::StatusCode::BAD_REQUEST,
                message,
            })
        });
    }

    Err(ParamsRejection(ActionResult::Error {
        format,
        status: axum::http::StatusCode::BAD_REQUEST,
        message: format!("unsupported content-type: {content_type}"),
    }))
}

fn json_object_to_string_map(value: serde_json::Value) -> Result<HashMap<String, String>, String> {
    let serde_json::Value::Object(map) = value else {
        return Err("json params body must be an object".to_string());
    };

    let mut params = HashMap::with_capacity(map.len());
    for (key, value) in map {
        match value {
            serde_json::Value::Null => {}
            serde_json::Value::String(value) => {
                params.insert(key, value);
            }
            serde_json::Value::Number(value) => {
                params.insert(key, value.to_string());
            }
            serde_json::Value::Bool(value) => {
                params.insert(key, value.to_string());
            }
            serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
                return Err(format!("json param `{key}` must be a scalar value"));
            }
        }
    }

    Ok(params)
}
