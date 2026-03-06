use std::{
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use sea_orm::entity::prelude::DateTime;
use sha2::{Digest, Sha256};

use crate::entity::hyperlink_artifact::HyperlinkArtifactKind;

const DEFAULT_ARTIFACTS_DIR: &str = "artifacts";
pub const ARTIFACTS_DIR_ENV: &str = "ARTIFACTS_DIR";
pub const DISK_STORAGE_BACKEND: &str = "disk";

static ARTIFACT_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct StoredArtifact {
    pub storage_path: String,
    pub checksum_sha256: String,
}

pub fn artifacts_root() -> PathBuf {
    std::env::var(ARTIFACTS_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_ARTIFACTS_DIR))
}

pub async fn write_payload(
    hyperlink_id: i32,
    kind: &HyperlinkArtifactKind,
    created_at: DateTime,
    payload: &[u8],
) -> Result<StoredArtifact, String> {
    let root = artifacts_root();
    let relative = build_relative_path(hyperlink_id, kind, created_at);
    let absolute = root.join(&relative);
    let parent = absolute
        .parent()
        .ok_or_else(|| "artifact path missing parent directory".to_string())?;

    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|err| format!("failed to create artifact directory {parent:?}: {err}"))?;
    tokio::fs::write(&absolute, payload)
        .await
        .map_err(|err| format!("failed to write artifact file {absolute:?}: {err}"))?;

    Ok(StoredArtifact {
        storage_path: relative,
        checksum_sha256: sha256_hex(payload),
    })
}

pub async fn read_payload(storage_path: &str) -> Result<Vec<u8>, String> {
    let relative = validated_relative_path(storage_path)?;
    let absolute = artifacts_root().join(relative);
    tokio::fs::read(&absolute)
        .await
        .map_err(|err| format!("failed to read artifact file {absolute:?}: {err}"))
}

pub async fn delete_if_exists(storage_path: &str) -> Result<bool, String> {
    let relative = validated_relative_path(storage_path)?;
    let absolute = artifacts_root().join(relative);

    match tokio::fs::remove_file(&absolute).await {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(format!(
            "failed to delete artifact file {absolute:?}: {err}"
        )),
    }
}

fn build_relative_path(
    hyperlink_id: i32,
    kind: &HyperlinkArtifactKind,
    created_at: DateTime,
) -> String {
    let timestamp = created_at.format("%Y%m%dT%H%M%SZ");
    let year = created_at.format("%Y");
    let month = created_at.format("%m");
    let day = created_at.format("%d");
    let unique = unique_suffix();
    let kind_slug = kind_slug(kind);
    let extension = extension(kind);

    format!("{hyperlink_id}/{kind_slug}/{year}/{month}/{day}/{timestamp}-{unique}.{extension}")
}

fn validated_relative_path(storage_path: &str) -> Result<&Path, String> {
    let path = Path::new(storage_path);
    if path.is_absolute() {
        return Err("artifact storage path must be relative".to_string());
    }

    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            _ => return Err("artifact storage path contains unsafe components".to_string()),
        }
    }

    Ok(path)
}

fn unique_suffix() -> String {
    let seq = ARTIFACT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}-{seq:x}")
}

fn kind_slug(kind: &HyperlinkArtifactKind) -> &'static str {
    match kind {
        HyperlinkArtifactKind::SnapshotWarc => "snapshot_warc",
        HyperlinkArtifactKind::PdfSource => "pdf_source",
        HyperlinkArtifactKind::PaperlessMetadata => "paperless_metadata",
        HyperlinkArtifactKind::SnapshotError => "snapshot_error",
        HyperlinkArtifactKind::OembedMeta => "oembed_meta",
        HyperlinkArtifactKind::OembedError => "oembed_error",
        HyperlinkArtifactKind::OgMeta => "og_meta",
        HyperlinkArtifactKind::OgImage => "og_image",
        HyperlinkArtifactKind::OgError => "og_error",
        HyperlinkArtifactKind::ReadableText => "readable_text",
        HyperlinkArtifactKind::ReadableMeta => "readable_meta",
        HyperlinkArtifactKind::ReadableError => "readable_error",
        HyperlinkArtifactKind::ScreenshotWebp => "screenshot_webp",
        HyperlinkArtifactKind::ScreenshotThumbWebp => "screenshot_thumb_webp",
        HyperlinkArtifactKind::ScreenshotDarkWebp => "screenshot_dark_webp",
        HyperlinkArtifactKind::ScreenshotThumbDarkWebp => "screenshot_thumb_dark_webp",
        HyperlinkArtifactKind::ScreenshotError => "screenshot_error",
        HyperlinkArtifactKind::TagMeta => "tag_meta",
    }
}

fn extension(kind: &HyperlinkArtifactKind) -> &'static str {
    match kind {
        HyperlinkArtifactKind::SnapshotWarc => "warc",
        HyperlinkArtifactKind::PdfSource => "pdf",
        HyperlinkArtifactKind::PaperlessMetadata => "json",
        HyperlinkArtifactKind::SnapshotError => "json",
        HyperlinkArtifactKind::OembedMeta => "json",
        HyperlinkArtifactKind::OembedError => "json",
        HyperlinkArtifactKind::OgMeta => "json",
        HyperlinkArtifactKind::OgImage => "img",
        HyperlinkArtifactKind::OgError => "json",
        HyperlinkArtifactKind::ReadableText => "md",
        HyperlinkArtifactKind::ReadableMeta => "json",
        HyperlinkArtifactKind::ReadableError => "json",
        HyperlinkArtifactKind::ScreenshotWebp => "webp",
        HyperlinkArtifactKind::ScreenshotThumbWebp => "webp",
        HyperlinkArtifactKind::ScreenshotDarkWebp => "webp",
        HyperlinkArtifactKind::ScreenshotThumbDarkWebp => "webp",
        HyperlinkArtifactKind::ScreenshotError => "json",
        HyperlinkArtifactKind::TagMeta => "json",
    }
}

fn sha256_hex(payload: &[u8]) -> String {
    let digest = Sha256::digest(payload);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}
