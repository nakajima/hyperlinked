use super::*;

fn artifact_with_payload(
    kind: HyperlinkArtifactKind,
    payload: Vec<u8>,
    content_type: &str,
) -> hyperlink_artifact::Model {
    hyperlink_artifact::Model {
        id: 1,
        hyperlink_id: 1,
        job_id: None,
        kind,
        payload,
        storage_path: None,
        storage_backend: None,
        checksum_sha256: None,
        content_type: content_type.to_string(),
        size_bytes: 0,
        created_at: now_utc(),
    }
}

#[test]
fn gzip_compress_round_trips_snapshot_payload() {
    let raw = b"WARC/1.0\r\nWARC-Type: response\r\n\r\n<html>hello</html>";
    let compressed = gzip_encode(raw).expect("snapshot warc should gzip");
    assert!(is_gzip_payload(&compressed));
    let decoded = gzip_decode(&compressed).expect("gzip payload should decode");
    assert_eq!(decoded, raw);
}

#[tokio::test]
async fn load_processing_payload_decodes_gzip_snapshot_warc() {
    let raw = b"WARC/1.0\r\nWARC-Type: response\r\n\r\n<html>hello</html>";
    let compressed = gzip_encode(raw).expect("snapshot warc should gzip");
    let artifact = artifact_with_payload(
        HyperlinkArtifactKind::SnapshotWarc,
        compressed,
        SNAPSHOT_WARC_GZIP_CONTENT_TYPE,
    );

    let payload = load_processing_payload(&artifact)
        .await
        .expect("processing payload should decode");
    assert_eq!(payload, raw);
}

#[tokio::test]
async fn load_processing_payload_keeps_non_gzip_snapshot_warc_unchanged() {
    let raw = b"WARC/1.0\r\nWARC-Type: response\r\n\r\n<html>hello</html>".to_vec();
    let artifact = artifact_with_payload(
        HyperlinkArtifactKind::SnapshotWarc,
        raw.clone(),
        SNAPSHOT_WARC_CONTENT_TYPE,
    );

    let payload = load_processing_payload(&artifact)
        .await
        .expect("processing payload should load");
    assert_eq!(payload, raw);
}
