use std::collections::HashMap;

use axum::{
    http::{
        HeaderMap, HeaderValue,
        header::{COOKIE, SET_COOKIE},
    },
    response::{IntoResponse, Redirect, Response},
};
use serde::{Deserialize, Serialize};

const FLASH_COOKIE_NAME: &str = "_hyperlinked_flash";
const FLASH_COOKIE_MAX_AGE_SECONDS: i64 = 60;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum FlashName {
    Notice,
    Alert,
}

#[derive(Clone, Debug, Default)]
pub struct Flash {
    entries: HashMap<FlashName, String>,
    cookie_present: bool,
    dirty: bool,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct FlashCookiePayload {
    #[serde(default)]
    notice: Option<String>,
    #[serde(default)]
    alert: Option<String>,
}

impl Flash {
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let Some(raw_cookie_value) = find_cookie(headers, FLASH_COOKIE_NAME) else {
            return Self::default();
        };

        let mut flash = Self {
            entries: HashMap::new(),
            cookie_present: true,
            dirty: false,
        };

        match decode_cookie_payload(&raw_cookie_value) {
            Some(payload) => {
                if let Some(notice) = payload.notice.filter(|value| !value.is_empty()) {
                    flash.entries.insert(FlashName::Notice, notice);
                }
                if let Some(alert) = payload.alert.filter(|value| !value.is_empty()) {
                    flash.entries.insert(FlashName::Alert, alert);
                }
            }
            None => {
                // Invalid payloads are cleared on the next response.
                flash.dirty = true;
            }
        }

        flash
    }

    pub fn insert(&mut self, named: FlashName, message: impl Into<String>) {
        self.entries.insert(named, message.into());
        self.dirty = true;
    }

    pub fn render_flash(&mut self, named: FlashName) -> Option<String> {
        let value = self.entries.remove(&named);
        if value.is_some() {
            self.dirty = true;
        }
        value
    }

    pub fn apply_to_response_headers(&self, headers: &mut HeaderMap) {
        let cookie = self.outgoing_cookie_header();
        let Some(cookie) = cookie else {
            return;
        };
        if let Ok(value) = HeaderValue::from_str(&cookie) {
            headers.append(SET_COOKIE, value);
        }
    }

    fn outgoing_cookie_header(&self) -> Option<String> {
        if self.entries.is_empty() {
            if self.cookie_present || self.dirty {
                return Some(delete_cookie_header_value());
            }
            return None;
        }

        if !self.dirty && self.cookie_present {
            return None;
        }

        encode_cookie_payload(&self.entries).map(|payload| set_cookie_header_value(&payload))
    }
}

pub fn redirect_with_flash(
    request_headers: &HeaderMap,
    location: &str,
    named: FlashName,
    message: impl Into<String>,
) -> Response {
    let mut flash = Flash::from_headers(request_headers);
    flash.insert(named, message);

    let mut response = Redirect::to(location).into_response();
    flash.apply_to_response_headers(response.headers_mut());
    response
}

fn find_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|cookie_header| {
            cookie_header.split(';').find_map(|segment| {
                let mut parts = segment.trim().splitn(2, '=');
                let key = parts.next()?.trim();
                let value = parts.next()?.trim();
                if key != name {
                    return None;
                }
                Some(value.trim_matches('"').to_string())
            })
        })
}

fn decode_cookie_payload(raw: &str) -> Option<FlashCookiePayload> {
    serde_urlencoded::from_str::<FlashCookiePayload>(raw).ok()
}

fn encode_cookie_payload(entries: &HashMap<FlashName, String>) -> Option<String> {
    let payload = FlashCookiePayload {
        notice: entries.get(&FlashName::Notice).cloned(),
        alert: entries.get(&FlashName::Alert).cloned(),
    };
    serde_urlencoded::to_string(payload).ok()
}

fn set_cookie_header_value(payload: &str) -> String {
    format!(
        "{FLASH_COOKIE_NAME}={payload}; Max-Age={FLASH_COOKIE_MAX_AGE_SECONDS}; Path=/; HttpOnly; SameSite=Lax"
    )
}

fn delete_cookie_header_value() -> String {
    format!("{FLASH_COOKIE_NAME}=; Max-Age=0; Path=/; HttpOnly; SameSite=Lax")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header;

    #[test]
    fn round_trips_flash_cookie_payload() {
        let mut flash = Flash::default();
        flash.insert(FlashName::Notice, "saved");
        flash.insert(FlashName::Alert, "bad");

        let header_value = flash
            .outgoing_cookie_header()
            .expect("cookie should be generated");
        assert!(header_value.contains("notice=saved"));
        assert!(header_value.contains("alert=bad"));
    }

    #[test]
    fn loads_flash_from_cookie_and_consumes_once() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("_hyperlinked_flash=notice=Saved+link.&alert=Oops"),
        );

        let mut flash = Flash::from_headers(&headers);
        assert_eq!(
            flash.render_flash(FlashName::Notice).as_deref(),
            Some("Saved link.")
        );
        assert_eq!(flash.render_flash(FlashName::Notice), None);
        assert_eq!(
            flash.render_flash(FlashName::Alert).as_deref(),
            Some("Oops")
        );
    }

    #[test]
    fn consumed_flash_deletes_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("_hyperlinked_flash=notice=Saved+link."),
        );

        let mut flash = Flash::from_headers(&headers);
        let _ = flash.render_flash(FlashName::Notice);
        let mut response_headers = HeaderMap::new();
        flash.apply_to_response_headers(&mut response_headers);

        let cookie = response_headers
            .get(header::SET_COOKIE)
            .expect("set-cookie should be present")
            .to_str()
            .expect("set-cookie should be valid");
        assert!(cookie.contains("_hyperlinked_flash="));
        assert!(cookie.contains("Max-Age=0"));
    }

    #[test]
    fn invalid_cookie_payload_is_cleared() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("_hyperlinked_flash=%%%invalid%%%"),
        );

        let flash = Flash::from_headers(&headers);
        let mut response_headers = HeaderMap::new();
        flash.apply_to_response_headers(&mut response_headers);

        let cookie = response_headers
            .get(header::SET_COOKIE)
            .expect("set-cookie should be present for invalid payload")
            .to_str()
            .expect("set-cookie should be valid");
        assert!(cookie.contains("Max-Age=0"));
    }
}
