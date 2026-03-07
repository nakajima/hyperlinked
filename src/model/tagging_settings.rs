use sea_orm::{DatabaseConnection, DbErr};

use crate::model::kv_store;

const KEY_ENABLED: &str = "settings.tags.enabled";
const KEY_PROVIDER: &str = "settings.tags.provider";
const KEY_BASE_URL: &str = "settings.tags.base_url";
const KEY_API_KEY: &str = "settings.tags.api_key";
const KEY_MODEL: &str = "settings.tags.model";
const KEY_AUTH_HEADER_NAME: &str = "settings.tags.auth_header_name";
const KEY_AUTH_HEADER_PREFIX: &str = "settings.tags.auth_header_prefix";
const KEY_BACKEND_KIND: &str = "settings.tags.backend_kind";
const KEY_VOCABULARY_JSON: &str = "settings.tags.vocabulary_json";

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_MODEL: &str = "gpt-4.1-mini";
const DEFAULT_AUTH_HEADER_NAME: &str = "Authorization";
const DEFAULT_AUTH_HEADER_PREFIX: &str = "Bearer";
const DEFAULT_VOCABULARY: [&str; 5] = ["build", "learn", "reference", "buy", "share"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaggingProvider {
    OpenAiCompatible,
}

impl TaggingProvider {
    fn from_storage(raw: Option<&str>) -> Self {
        match raw.unwrap_or_default().trim().to_ascii_lowercase().as_str() {
            "openai_compatible" => Self::OpenAiCompatible,
            _ => Self::OpenAiCompatible,
        }
    }

    pub fn as_storage(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "openai_compatible",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaggingBackendKind {
    OpenAiCompatible,
    Ollama,
    Unknown,
}

impl TaggingBackendKind {
    pub fn from_storage(raw: Option<&str>) -> Self {
        match raw.unwrap_or_default().trim().to_ascii_lowercase().as_str() {
            "openai_compatible" | "openai_v1" => Self::OpenAiCompatible,
            "ollama" | "ollama_api" => Self::Ollama,
            _ => Self::Unknown,
        }
    }

    pub fn as_storage(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "openai_compatible",
            Self::Ollama => "ollama",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaggingSettings {
    pub enabled: bool,
    pub provider: TaggingProvider,
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub auth_header_name: Option<String>,
    pub auth_header_prefix: Option<String>,
    pub backend_kind: TaggingBackendKind,
    pub vocabulary: Vec<String>,
}

impl Default for TaggingSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: TaggingProvider::OpenAiCompatible,
            base_url: DEFAULT_BASE_URL.to_string(),
            api_key: None,
            model: DEFAULT_MODEL.to_string(),
            auth_header_name: Some(DEFAULT_AUTH_HEADER_NAME.to_string()),
            auth_header_prefix: Some(DEFAULT_AUTH_HEADER_PREFIX.to_string()),
            backend_kind: TaggingBackendKind::Unknown,
            vocabulary: DEFAULT_VOCABULARY
                .iter()
                .map(|value| value.to_string())
                .collect(),
        }
    }
}

impl TaggingSettings {
    pub fn normalized(self) -> Self {
        let provider = self.provider;
        let base_url = parse_non_empty(Some(self.base_url)).unwrap_or_else(|| match provider {
            TaggingProvider::OpenAiCompatible => DEFAULT_BASE_URL.to_string(),
        });
        let model = parse_non_empty(Some(self.model)).unwrap_or_else(|| DEFAULT_MODEL.to_string());
        let api_key = parse_non_empty(self.api_key);
        let auth_header_name = parse_non_empty(self.auth_header_name);
        let auth_header_prefix = parse_non_empty(self.auth_header_prefix);
        let backend_kind = self.backend_kind;
        let vocabulary = normalize_vocabulary(self.vocabulary);

        Self {
            enabled: self.enabled,
            provider,
            base_url,
            api_key,
            model,
            auth_header_name,
            auth_header_prefix,
            backend_kind,
            vocabulary,
        }
    }

    pub fn classification_enabled(&self) -> bool {
        self.enabled && !self.model.trim().is_empty() && !self.vocabulary.is_empty()
    }
}

pub async fn load(connection: &DatabaseConnection) -> Result<TaggingSettings, DbErr> {
    let defaults = TaggingSettings::default();
    let values = kv_store::get_many(
        connection,
        &[
            KEY_ENABLED,
            KEY_PROVIDER,
            KEY_BASE_URL,
            KEY_API_KEY,
            KEY_MODEL,
            KEY_AUTH_HEADER_NAME,
            KEY_AUTH_HEADER_PREFIX,
            KEY_BACKEND_KIND,
            KEY_VOCABULARY_JSON,
        ],
    )
    .await?;

    let vocabulary = parse_vocabulary_json(values.get(KEY_VOCABULARY_JSON).map(String::as_str))
        .unwrap_or_else(|| defaults.vocabulary.clone());

    Ok(TaggingSettings {
        enabled: parse_bool(
            values.get(KEY_ENABLED).map(String::as_str),
            defaults.enabled,
        ),
        provider: TaggingProvider::from_storage(values.get(KEY_PROVIDER).map(String::as_str)),
        base_url: values
            .get(KEY_BASE_URL)
            .cloned()
            .unwrap_or_else(|| defaults.base_url.clone()),
        api_key: values.get(KEY_API_KEY).cloned(),
        model: values
            .get(KEY_MODEL)
            .cloned()
            .unwrap_or_else(|| defaults.model.clone()),
        auth_header_name: match values.get(KEY_AUTH_HEADER_NAME) {
            Some(value) => parse_non_empty(Some(value.clone())),
            None => defaults.auth_header_name.as_ref().map(ToString::to_string),
        },
        auth_header_prefix: match values.get(KEY_AUTH_HEADER_PREFIX) {
            Some(value) => parse_non_empty(Some(value.clone())),
            None => defaults
                .auth_header_prefix
                .as_ref()
                .map(ToString::to_string),
        },
        backend_kind: TaggingBackendKind::from_storage(
            values.get(KEY_BACKEND_KIND).map(String::as_str),
        ),
        vocabulary,
    }
    .normalized())
}

pub async fn save(
    connection: &DatabaseConnection,
    settings: TaggingSettings,
) -> Result<TaggingSettings, DbErr> {
    let settings = settings.normalized();

    kv_store::set(connection, KEY_ENABLED, bool_to_storage(settings.enabled)).await?;
    kv_store::set(connection, KEY_PROVIDER, settings.provider.as_storage()).await?;
    kv_store::set(connection, KEY_BASE_URL, &settings.base_url).await?;
    kv_store::set(connection, KEY_MODEL, &settings.model).await?;
    kv_store::set(
        connection,
        KEY_VOCABULARY_JSON,
        &serde_json::to_string(&settings.vocabulary).unwrap_or_else(|_| "[]".to_string()),
    )
    .await?;

    save_optional(connection, KEY_API_KEY, settings.api_key.as_deref()).await?;
    save_optional_or_blank(
        connection,
        KEY_AUTH_HEADER_NAME,
        settings.auth_header_name.as_deref(),
    )
    .await?;
    save_optional_or_blank(
        connection,
        KEY_AUTH_HEADER_PREFIX,
        settings.auth_header_prefix.as_deref(),
    )
    .await?;
    kv_store::set(connection, KEY_BACKEND_KIND, settings.backend_kind.as_storage()).await?;

    Ok(settings)
}

pub fn parse_vocabulary_lines(raw: &str) -> Vec<String> {
    normalize_vocabulary(
        raw.lines()
            .flat_map(|line| line.split(','))
            .map(|token| token.to_string())
            .collect(),
    )
}

fn parse_vocabulary_json(raw: Option<&str>) -> Option<Vec<String>> {
    let raw = raw?;
    let parsed = serde_json::from_str::<Vec<String>>(raw).ok()?;
    Some(normalize_vocabulary(parsed))
}

fn normalize_vocabulary(values: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for value in values {
        let Some(value) = parse_non_empty(Some(value)) else {
            continue;
        };
        let key = value.to_ascii_lowercase();
        if seen.insert(key) {
            normalized.push(value);
        }
    }

    if normalized.is_empty() {
        return DEFAULT_VOCABULARY
            .iter()
            .map(|value| value.to_string())
            .collect();
    }

    normalized
}

async fn save_optional(
    connection: &DatabaseConnection,
    key: &str,
    value: Option<&str>,
) -> Result<(), DbErr> {
    if let Some(value) = value {
        kv_store::set(connection, key, value).await
    } else {
        kv_store::delete(connection, key).await
    }
}

async fn save_optional_or_blank(
    connection: &DatabaseConnection,
    key: &str,
    value: Option<&str>,
) -> Result<(), DbErr> {
    kv_store::set(connection, key, value.unwrap_or("")).await
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

fn parse_non_empty(raw: Option<String>) -> Option<String> {
    raw.map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::test_support;

    #[tokio::test]
    async fn load_defaults_when_keys_are_missing() {
        let connection = test_support::new_memory_connection().await;
        let loaded = load(&connection).await.expect("settings should load");
        assert_eq!(loaded, TaggingSettings::default());
        assert!(!loaded.classification_enabled());
    }

    #[tokio::test]
    async fn save_normalizes_and_round_trips() {
        let connection = test_support::new_memory_connection().await;
        let saved = save(
            &connection,
            TaggingSettings {
                enabled: true,
                provider: TaggingProvider::OpenAiCompatible,
                base_url: "  ".to_string(),
                api_key: Some("   ".to_string()),
                model: "  custom-model  ".to_string(),
                auth_header_name: Some("   X-API-Key ".to_string()),
                auth_header_prefix: Some(" ".to_string()),
                backend_kind: TaggingBackendKind::Ollama,
                vocabulary: vec![
                    "learn".to_string(),
                    "LEARN".to_string(),
                    "reference".to_string(),
                ],
            },
        )
        .await
        .expect("settings should save");

        assert_eq!(saved.base_url, DEFAULT_BASE_URL);
        assert_eq!(saved.api_key, None);
        assert_eq!(saved.model, "custom-model");
        assert_eq!(saved.auth_header_name.as_deref(), Some("X-API-Key"));
        assert_eq!(saved.auth_header_prefix, None);
        assert_eq!(saved.backend_kind, TaggingBackendKind::Ollama);
        assert_eq!(
            saved.vocabulary,
            vec!["learn".to_string(), "reference".to_string()]
        );
        assert!(saved.classification_enabled());

        let loaded = load(&connection).await.expect("settings should load");
        assert_eq!(loaded, saved);
    }

    #[test]
    fn parse_vocabulary_lines_accepts_comma_or_newline_separated_values() {
        let parsed = parse_vocabulary_lines("learn, build\nreference\nlearn");
        assert_eq!(
            parsed,
            vec![
                "learn".to_string(),
                "build".to_string(),
                "reference".to_string()
            ]
        );
    }
}
