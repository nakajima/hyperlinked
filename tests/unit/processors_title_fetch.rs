use super::*;
use sea_orm::{ActiveValue::Set, Database};

#[test]
fn extracts_title_from_html() {
    let html = "<html><head><title>  Example   Title </title></head><body></body></html>";
    let title = extract_html_title(html);
    assert_eq!(title.as_deref(), Some("Example Title"));
}

#[test]
fn rejects_private_ip_hosts() {
    assert!(is_private_ip(
        "127.0.0.1".parse::<IpAddr>().expect("valid ip")
    ));
    assert!(is_private_ip(
        "10.0.0.5".parse::<IpAddr>().expect("valid ip")
    ));
    assert!(is_private_ip("::1".parse::<IpAddr>().expect("valid ip")));
    assert!(!is_private_ip(
        "8.8.8.8".parse::<IpAddr>().expect("valid ip")
    ));
}

#[tokio::test]
async fn process_ignores_relative_url_parse_errors() {
    let connection = Database::connect("sqlite::memory:")
        .await
        .expect("in-memory db should initialize");
    let mut hyperlink = hyperlink::ActiveModel {
        title: Set("Uploaded PDF".to_string()),
        url: Set("/uploads/1/file.pdf".to_string()),
        raw_url: Set("/uploads/1/file.pdf".to_string()),
        ..Default::default()
    };

    let result = TitleFetcher {}.process(&mut hyperlink, &connection).await;
    assert!(result.is_ok());
}
