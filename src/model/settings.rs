use sea_orm::{DatabaseConnection, DbErr};

use crate::{entity::hyperlink_processing_job::HyperlinkProcessingJobKind, model::kv_store};

const KEY_COLLECT_SOURCE: &str = "settings.artifacts.collect_source";
const KEY_COLLECT_SCREENSHOTS: &str = "settings.artifacts.collect_screenshots";
const KEY_COLLECT_SCREENSHOT_DARK: &str = "settings.artifacts.collect_screenshot_dark";
const KEY_COLLECT_OG: &str = "settings.artifacts.collect_og";
const KEY_COLLECT_READABILITY: &str = "settings.artifacts.collect_readability";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArtifactCollectionSettings {
    pub collect_source: bool,
    pub collect_screenshots: bool,
    pub collect_screenshot_dark: bool,
    pub collect_og: bool,
    pub collect_readability: bool,
}

impl Default for ArtifactCollectionSettings {
    fn default() -> Self {
        Self {
            collect_source: true,
            collect_screenshots: true,
            collect_screenshot_dark: true,
            collect_og: true,
            collect_readability: true,
        }
    }
}

impl ArtifactCollectionSettings {
    pub fn normalized(self) -> Self {
        if !self.collect_source {
            return Self {
                collect_source: false,
                collect_screenshots: false,
                collect_screenshot_dark: false,
                collect_og: false,
                collect_readability: false,
            };
        }

        if !self.collect_screenshots {
            return Self {
                collect_screenshot_dark: false,
                ..self
            };
        }

        self
    }

    pub fn allows_processing_job_kind(self, kind: HyperlinkProcessingJobKind) -> bool {
        match kind {
            HyperlinkProcessingJobKind::Snapshot => self.collect_source,
            HyperlinkProcessingJobKind::Og => self.collect_og,
            HyperlinkProcessingJobKind::Readability => self.collect_readability,
            HyperlinkProcessingJobKind::SublinkDiscovery
            | HyperlinkProcessingJobKind::Oembed
            | HyperlinkProcessingJobKind::TagClassification
            | HyperlinkProcessingJobKind::TagReclassify => true,
        }
    }
}

pub async fn load(connection: &DatabaseConnection) -> Result<ArtifactCollectionSettings, DbErr> {
    let defaults = ArtifactCollectionSettings::default();
    let values = kv_store::get_many(
        connection,
        &[
            KEY_COLLECT_SOURCE,
            KEY_COLLECT_SCREENSHOTS,
            KEY_COLLECT_SCREENSHOT_DARK,
            KEY_COLLECT_OG,
            KEY_COLLECT_READABILITY,
        ],
    )
    .await?;

    Ok(ArtifactCollectionSettings {
        collect_source: parse_bool(
            values.get(KEY_COLLECT_SOURCE).map(String::as_str),
            defaults.collect_source,
        ),
        collect_screenshots: parse_bool(
            values.get(KEY_COLLECT_SCREENSHOTS).map(String::as_str),
            defaults.collect_screenshots,
        ),
        collect_screenshot_dark: parse_bool(
            values.get(KEY_COLLECT_SCREENSHOT_DARK).map(String::as_str),
            defaults.collect_screenshot_dark,
        ),
        collect_og: parse_bool(
            values.get(KEY_COLLECT_OG).map(String::as_str),
            defaults.collect_og,
        ),
        collect_readability: parse_bool(
            values.get(KEY_COLLECT_READABILITY).map(String::as_str),
            defaults.collect_readability,
        ),
    }
    .normalized())
}

pub async fn save(
    connection: &DatabaseConnection,
    settings: ArtifactCollectionSettings,
) -> Result<ArtifactCollectionSettings, DbErr> {
    let settings = settings.normalized();
    kv_store::set(
        connection,
        KEY_COLLECT_SOURCE,
        bool_to_storage(settings.collect_source),
    )
    .await?;
    kv_store::set(
        connection,
        KEY_COLLECT_SCREENSHOTS,
        bool_to_storage(settings.collect_screenshots),
    )
    .await?;
    kv_store::set(
        connection,
        KEY_COLLECT_SCREENSHOT_DARK,
        bool_to_storage(settings.collect_screenshot_dark),
    )
    .await?;
    kv_store::set(
        connection,
        KEY_COLLECT_OG,
        bool_to_storage(settings.collect_og),
    )
    .await?;
    kv_store::set(
        connection,
        KEY_COLLECT_READABILITY,
        bool_to_storage(settings.collect_readability),
    )
    .await?;
    Ok(settings)
}

fn bool_to_storage(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn parse_bool(raw: Option<&str>, default: bool) -> bool {
    let Some(raw) = raw else {
        return default;
    };

    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => true,
        "false" | "0" | "no" | "off" => false,
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::test_support;

    #[tokio::test]
    async fn load_defaults_when_keys_are_missing() {
        let connection = test_support::new_memory_connection().await;
        let loaded = load(&connection).await.expect("settings should load");
        assert_eq!(loaded, ArtifactCollectionSettings::default());
    }

    #[tokio::test]
    async fn save_normalizes_dependent_flags() {
        let connection = test_support::new_memory_connection().await;
        let saved = save(
            &connection,
            ArtifactCollectionSettings {
                collect_source: false,
                collect_screenshots: true,
                collect_screenshot_dark: true,
                collect_og: true,
                collect_readability: true,
            },
        )
        .await
        .expect("settings should save");

        assert_eq!(
            saved,
            ArtifactCollectionSettings {
                collect_source: false,
                collect_screenshots: false,
                collect_screenshot_dark: false,
                collect_og: false,
                collect_readability: false,
            }
        );

        let loaded = load(&connection).await.expect("settings should load");
        assert_eq!(loaded, saved);
    }
}
