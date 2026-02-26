use reqwest::{Url, header::CONTENT_TYPE};
use sea_orm::ActiveValue::Set;
use sea_orm::DatabaseConnection;
use std::net::IpAddr;
use std::time::Duration;
use tokio::net::lookup_host;

use crate::entity::hyperlink;
use crate::model::hyperlink_title;
use crate::processors::processor::{ProcessingError, Processor};

pub struct TitleFetcher {}

impl Processor for TitleFetcher {
    type Output = ();

    async fn process<'a>(
        &'a mut self,
        hyperlink: &'a mut hyperlink::ActiveModel,
        _connection: &'a DatabaseConnection,
    ) -> Result<Self::Output, super::processor::ProcessingError> {
        if let Some(title) = fetch_title_from_url(hyperlink.url.as_ref())
            .await
            .map_err(ProcessingError::FetchError)?
        {
            let cleaned_title = hyperlink_title::strip_site_affixes(
                title.as_str(),
                hyperlink.url.as_ref(),
                hyperlink.raw_url.as_ref(),
            );
            hyperlink.title = Set(cleaned_title);
        }
        Ok(())
    }
}

async fn fetch_title_from_url(url: &str) -> Result<Option<String>, String> {
    let parsed = Url::parse(url).map_err(|err| format!("invalid url: {err}"))?;
    ensure_fetchable_url(&parsed).await?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(4))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|err| format!("failed to build http client: {err}"))?;

    let mut response = client
        .get(parsed)
        .send()
        .await
        .map_err(|err| format!("failed to fetch title: {err}"))?;

    if !response.status().is_success() {
        return Ok(None);
    }

    if let Some(content_type) = response.headers().get(CONTENT_TYPE) {
        let content_type = content_type.to_str().unwrap_or_default().to_lowercase();
        if !content_type.contains("text/html") && !content_type.contains("application/xhtml+xml") {
            return Ok(None);
        }
    }

    const MAX_BYTES: usize = 256 * 1024;
    let mut body = Vec::with_capacity(4096);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| format!("failed to read response body: {err}"))?
    {
        if body.len() >= MAX_BYTES {
            break;
        }

        let remaining = MAX_BYTES - body.len();
        if chunk.len() > remaining {
            body.extend_from_slice(&chunk[..remaining]);
            break;
        }

        body.extend_from_slice(&chunk);
    }

    let html = String::from_utf8_lossy(&body);
    Ok(extract_html_title(&html))
}

async fn ensure_fetchable_url(url: &Url) -> Result<(), String> {
    match url.scheme() {
        "http" | "https" => {}
        _ => return Err("only http/https URLs are supported".to_string()),
    }

    let host = url
        .host_str()
        .ok_or_else(|| "url host is missing".to_string())?;
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return Err("localhost URLs are not allowed".to_string());
    }

    let port = url.port_or_known_default().unwrap_or(80);
    let resolved = lookup_host((host, port))
        .await
        .map_err(|err| format!("failed to resolve host: {err}"))?;

    for addr in resolved {
        if is_private_ip(addr.ip()) {
            return Err("private or loopback addresses are not allowed".to_string());
        }
    }

    Ok(())
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ipv4) => {
            ipv4.is_private()
                || ipv4.is_loopback()
                || ipv4.is_link_local()
                || ipv4.is_unspecified()
                || ipv4.is_broadcast()
                || ipv4.is_documentation()
                || ipv4.is_multicast()
                || ipv4.octets()[0] == 0
        }
        IpAddr::V6(ipv6) => {
            ipv6.is_loopback()
                || ipv6.is_unspecified()
                || ipv6.is_unique_local()
                || ipv6.is_unicast_link_local()
                || ipv6.is_multicast()
        }
    }
}

fn extract_html_title(document: &str) -> Option<String> {
    let lowercase = document.to_lowercase();
    let title_start = lowercase.find("<title")?;
    let open_end = lowercase[title_start..].find('>')?;
    let content_start = title_start + open_end + 1;
    let close_start = lowercase[content_start..].find("</title>")?;
    let raw_title = &document[content_start..content_start + close_start];
    let normalized = raw_title.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
