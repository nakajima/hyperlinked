use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    entity::prelude::{DateTime, DateTimeUtc},
};
use sha2::{Digest, Sha256};

use crate::{
    app::models::hyperlink_processing_job,
    entity::{
        hyperlink,
        hyperlink_artifact::{self, HyperlinkArtifactKind},
        hyperlink_processing_job as hyperlink_processing_job_entity,
    },
};

pub(crate) const UPLOADS_PREFIX: &str = "/uploads";
pub(crate) const DEFAULT_FILENAME: &str = "document.pdf";

const DEFAULT_TITLE: &str = "Untitled PDF";
const PDF_SIGNATURE: &[u8] = b"%PDF-";

pub(crate) fn looks_like_pdf(payload: &[u8]) -> bool {
    payload.len() >= PDF_SIGNATURE.len() && &payload[..PDF_SIGNATURE.len()] == PDF_SIGNATURE
}

pub(crate) async fn latest_job_optional(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
) -> Option<hyperlink_processing_job_entity::Model> {
    hyperlink_processing_job::latest_for_hyperlink(connection, hyperlink_id)
        .await
        .ok()
        .flatten()
}

pub(crate) async fn find_existing_pdf_upload(
    connection: &DatabaseConnection,
    checksum_sha256: &str,
    filename: &str,
) -> Option<hyperlink::Model> {
    let artifacts = hyperlink_artifact::Entity::find()
        .filter(hyperlink_artifact::Column::Kind.eq(HyperlinkArtifactKind::PdfSource))
        .filter(hyperlink_artifact::Column::ChecksumSha256.eq(checksum_sha256.to_string()))
        .order_by_desc(hyperlink_artifact::Column::CreatedAt)
        .order_by_desc(hyperlink_artifact::Column::Id)
        .all(connection)
        .await
        .ok()?;

    for artifact in artifacts {
        let Some(link) = hyperlink::Entity::find_by_id(artifact.hyperlink_id)
            .one(connection)
            .await
            .ok()
            .flatten()
        else {
            continue;
        };
        if upload_filename_from_url(link.url.as_str()).as_deref() == Some(filename) {
            return Some(link);
        }
    }

    None
}

pub(crate) fn normalized_upload_title(raw_title: Option<&str>, filename: &str) -> String {
    if let Some(raw_title) = raw_title {
        let trimmed = raw_title.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    filename
        .strip_suffix(".pdf")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_TITLE)
        .to_string()
}

pub(crate) fn sanitize_pdf_filename(raw: &str) -> String {
    let trimmed = raw.trim();
    let without_query = trimmed.split(['?', '#']).next().unwrap_or(trimmed);
    let last_component = without_query
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(without_query);
    let mut cleaned = last_component
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | ' ' | '(' | ')'))
        .collect::<String>();

    cleaned = cleaned.trim().trim_matches('.').to_string();
    if cleaned.is_empty() {
        cleaned = DEFAULT_FILENAME.to_string();
    }

    if !cleaned.to_ascii_lowercase().ends_with(".pdf") {
        cleaned.push_str(".pdf");
    }

    while cleaned.contains("..") {
        cleaned = cleaned.replace("..", ".");
    }

    cleaned
}

pub(crate) fn sha256_hex(payload: &[u8]) -> String {
    let digest = Sha256::digest(payload);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(hex_char((byte >> 4) & 0x0F));
        output.push(hex_char(byte & 0x0F));
    }
    output
}

pub(crate) fn upload_hyperlink_url(id: i32, filename: &str) -> String {
    format!("{UPLOADS_PREFIX}/{id}/{filename}")
}

pub(crate) fn pending_upload_placeholder(filename: &str) -> String {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{UPLOADS_PREFIX}/pending-{nonce}/{filename}")
}

pub(crate) fn upload_filename_from_url(url: &str) -> Option<String> {
    let path = if url.starts_with('/') {
        url.split(['?', '#']).next().unwrap_or(url).to_string()
    } else {
        let parsed = reqwest::Url::parse(url).ok()?;
        parsed.path().to_string()
    };

    let mut parts = path.trim_start_matches('/').split('/');
    if parts.next()? != "uploads" {
        return None;
    }
    let _id = parts.next()?;
    let filename = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    Some(filename.to_string())
}

pub(crate) fn now_utc() -> DateTime {
    DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}

fn hex_char(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        10..=15 => (b'a' + (value - 10)) as char,
        _ => unreachable!("hex nibble must be in range 0..=15"),
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};

    use super::*;
    use crate::{
        entity::{
            hyperlink,
            hyperlink_artifact::{self, HyperlinkArtifactKind},
        },
        test_support,
    };

    #[test]
    fn looks_like_pdf_checks_signature_prefix() {
        assert!(looks_like_pdf(b"%PDF-1.7\nrest"));
        assert!(!looks_like_pdf(b"not a pdf"));
    }

    #[test]
    fn normalized_upload_title_prefers_explicit_title_then_filename_stem() {
        assert_eq!(
            normalized_upload_title(Some("  Quarterly Report  "), "ignored.pdf"),
            "Quarterly Report"
        );
        assert_eq!(normalized_upload_title(None, "paper.pdf"), "paper");
        assert_eq!(
            normalized_upload_title(Some("   "), "document.pdf"),
            "document"
        );
    }

    #[test]
    fn sanitize_pdf_filename_strips_paths_and_restores_extension() {
        assert_eq!(
            sanitize_pdf_filename("../tmp/Quarterly Report?download=1"),
            "Quarterly Report.pdf"
        );
        assert_eq!(sanitize_pdf_filename("..."), DEFAULT_FILENAME);
        assert_eq!(sanitize_pdf_filename("report.final"), "report.final.pdf");
    }

    #[test]
    fn upload_urls_round_trip_filename() {
        let url = upload_hyperlink_url(42, "paper.pdf");
        assert_eq!(url, "/uploads/42/paper.pdf");
        assert_eq!(upload_filename_from_url(&url).as_deref(), Some("paper.pdf"));
        assert_eq!(
            upload_filename_from_url("https://example.com/uploads/42/paper.pdf?download=1")
                .as_deref(),
            Some("paper.pdf")
        );
    }

    #[test]
    fn sha256_hex_outputs_lowercase_digest() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[tokio::test]
    async fn find_existing_pdf_upload_matches_checksum_and_filename() {
        let connection = test_support::new_memory_connection().await;
        test_support::initialize_hyperlinks_schema(&connection).await;

        let now = now_utc();
        let later = now + chrono::Duration::seconds(1);

        let matching_link = hyperlink::ActiveModel {
            id: Set(1),
            title: Set("Matching PDF".to_string()),
            url: Set(upload_hyperlink_url(1, "paper.pdf")),
            raw_url: Set(upload_hyperlink_url(1, "paper.pdf")),
            discovery_depth: Set(0),
            clicks_count: Set(0),
            source_type: Set(hyperlink::HyperlinkSourceType::Pdf),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&connection)
        .await
        .expect("matching hyperlink should insert");

        hyperlink_artifact::ActiveModel {
            hyperlink_id: Set(matching_link.id),
            job_id: Set(None),
            kind: Set(HyperlinkArtifactKind::PdfSource),
            payload: Set(Vec::new()),
            storage_path: Set(None),
            storage_backend: Set(None),
            checksum_sha256: Set(Some("same-checksum".to_string())),
            content_type: Set("application/pdf".to_string()),
            size_bytes: Set(32),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(&connection)
        .await
        .expect("matching artifact should insert");

        let non_matching_link = hyperlink::ActiveModel {
            id: Set(2),
            title: Set("Other PDF".to_string()),
            url: Set(upload_hyperlink_url(2, "other.pdf")),
            raw_url: Set(upload_hyperlink_url(2, "other.pdf")),
            discovery_depth: Set(0),
            clicks_count: Set(0),
            source_type: Set(hyperlink::HyperlinkSourceType::Pdf),
            created_at: Set(later),
            updated_at: Set(later),
            ..Default::default()
        }
        .insert(&connection)
        .await
        .expect("non-matching hyperlink should insert");

        hyperlink_artifact::ActiveModel {
            hyperlink_id: Set(non_matching_link.id),
            job_id: Set(None),
            kind: Set(HyperlinkArtifactKind::PdfSource),
            payload: Set(Vec::new()),
            storage_path: Set(None),
            storage_backend: Set(None),
            checksum_sha256: Set(Some("same-checksum".to_string())),
            content_type: Set("application/pdf".to_string()),
            size_bytes: Set(64),
            created_at: Set(later),
            ..Default::default()
        }
        .insert(&connection)
        .await
        .expect("non-matching artifact should insert");

        let found = find_existing_pdf_upload(&connection, "same-checksum", "paper.pdf")
            .await
            .expect("matching hyperlink should be found");
        assert_eq!(found.id, matching_link.id);
        assert_eq!(found.url, "/uploads/1/paper.pdf");
    }
}
