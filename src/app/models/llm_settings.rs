use sea_orm::{DatabaseConnection, DbErr};

use crate::app::models::kv_store;

const KEY_BASE_URL: &str = "settings.llm.base_url";
const KEY_API_KEY: &str = "settings.llm.api_key";
const KEY_MODEL: &str = "settings.llm.model";
const KEY_AUTH_HEADER_NAME: &str = "settings.llm.auth_header_name";
const KEY_AUTH_HEADER_PREFIX: &str = "settings.llm.auth_header_prefix";
const KEY_BACKEND_KIND: &str = "settings.llm.backend_kind";

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_MODEL: &str = "gpt-4.1-mini";
const DEFAULT_AUTH_HEADER_NAME: &str = "Authorization";
const DEFAULT_AUTH_HEADER_PREFIX: &str = "Bearer";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LlmProvider {
    OpenAiCompatible,
}

impl LlmProvider {
    pub fn as_storage(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "openai_compatible",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LlmBackendKind {
    OpenAiCompatible,
    Ollama,
    Unknown,
}

impl LlmBackendKind {
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
pub struct LlmSettings {
    pub provider: LlmProvider,
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub auth_header_name: Option<String>,
    pub auth_header_prefix: Option<String>,
    pub backend_kind: LlmBackendKind,
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            provider: LlmProvider::OpenAiCompatible,
            base_url: DEFAULT_BASE_URL.to_string(),
            api_key: None,
            model: DEFAULT_MODEL.to_string(),
            auth_header_name: Some(DEFAULT_AUTH_HEADER_NAME.to_string()),
            auth_header_prefix: Some(DEFAULT_AUTH_HEADER_PREFIX.to_string()),
            backend_kind: LlmBackendKind::Unknown,
        }
    }
}

impl LlmSettings {
    pub fn normalized(self) -> Self {
        Self {
            provider: self.provider,
            base_url: parse_non_empty(Some(self.base_url))
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            api_key: parse_non_empty(self.api_key),
            model: parse_non_empty(Some(self.model)).unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            auth_header_name: parse_non_empty(self.auth_header_name)
                .or_else(|| Some(DEFAULT_AUTH_HEADER_NAME.to_string())),
            auth_header_prefix: parse_non_empty(self.auth_header_prefix)
                .or_else(|| Some(DEFAULT_AUTH_HEADER_PREFIX.to_string())),
            backend_kind: self.backend_kind,
        }
    }
}

pub async fn load(connection: &DatabaseConnection) -> Result<LlmSettings, DbErr> {
    let defaults = LlmSettings::default();

    Ok(LlmSettings {
        provider: LlmProvider::OpenAiCompatible,
        base_url: kv_store::get(connection, KEY_BASE_URL)
            .await?
            .unwrap_or_else(|| defaults.base_url.clone()),
        api_key: kv_store::get(connection, KEY_API_KEY).await?,
        model: kv_store::get(connection, KEY_MODEL)
            .await?
            .unwrap_or_else(|| defaults.model.clone()),
        auth_header_name: kv_store::get(connection, KEY_AUTH_HEADER_NAME)
            .await?
            .or(defaults.auth_header_name.clone()),
        auth_header_prefix: kv_store::get(connection, KEY_AUTH_HEADER_PREFIX)
            .await?
            .or(defaults.auth_header_prefix.clone()),
        backend_kind: LlmBackendKind::from_storage(
            kv_store::get(connection, KEY_BACKEND_KIND)
                .await?
                .as_deref(),
        ),
    }
    .normalized())
}

pub async fn save(
    connection: &DatabaseConnection,
    settings: LlmSettings,
) -> Result<LlmSettings, DbErr> {
    let settings = settings.normalized();
    kv_store::set(connection, KEY_BASE_URL, &settings.base_url).await?;
    kv_store::set(connection, KEY_MODEL, &settings.model).await?;
    kv_store::set(
        connection,
        KEY_BACKEND_KIND,
        settings.backend_kind.as_storage(),
    )
    .await?;

    if let Some(api_key) = settings.api_key.as_deref() {
        kv_store::set(connection, KEY_API_KEY, api_key).await?;
    } else {
        kv_store::delete(connection, KEY_API_KEY).await?;
    }

    if let Some(auth_header_name) = settings.auth_header_name.as_deref() {
        kv_store::set(connection, KEY_AUTH_HEADER_NAME, auth_header_name).await?;
    } else {
        kv_store::delete(connection, KEY_AUTH_HEADER_NAME).await?;
    }

    if let Some(auth_header_prefix) = settings.auth_header_prefix.as_deref() {
        kv_store::set(connection, KEY_AUTH_HEADER_PREFIX, auth_header_prefix).await?;
    } else {
        kv_store::delete(connection, KEY_AUTH_HEADER_PREFIX).await?;
    }

    Ok(settings)
}

fn parse_non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}
#[cfg(test)]
#[path = "../../../tests/unit/app_models_llm_settings.rs"]
mod tests;
