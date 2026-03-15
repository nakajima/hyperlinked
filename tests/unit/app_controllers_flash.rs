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
