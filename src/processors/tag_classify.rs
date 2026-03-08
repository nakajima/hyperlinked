use std::{collections::HashSet, error::Error as _, time::Duration};

use chrono::Utc;
use reqwest::{
    Url,
    header::{CONTENT_TYPE, HeaderName, HeaderValue},
};
use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    entity::hyperlink,
    model::{
        hyperlink_tagging::{self, LlmPersistInput, PersistedRankedTag, RankedTag, TagState},
        tagging_settings::{TaggingBackendKind, TaggingProvider, TaggingSettings},
    },
    processors::processor::{ProcessingError, Processor},
};

const TAG_CLASSIFY_TIMEOUT: Duration = Duration::from_secs(20);
const MINIMUM_TAG_CONFIDENCE: f32 = 0.35;

pub struct TagClassifier {
    job_id: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TagClassificationMode {
    VocabularyOnly,
    DiscoverWithPending,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TagClassificationOutput {
    pub classified: bool,
    pub skipped_reason: Option<String>,
    pub tag_count: usize,
}

impl TagClassifier {
    pub fn new(job_id: i32) -> Self {
        Self { job_id }
    }
}

impl Processor for TagClassifier {
    type Output = TagClassificationOutput;

    async fn process<'a>(
        &'a mut self,
        hyperlink: &'a mut hyperlink::ActiveModel,
        connection: &'a DatabaseConnection,
    ) -> Result<Self::Output, ProcessingError> {
        classify_hyperlink(
            connection,
            hyperlink,
            Some(self.job_id),
            TagClassificationMode::VocabularyOnly,
        )
        .await
    }
}

pub async fn classify_hyperlink(
    connection: &DatabaseConnection,
    hyperlink: &mut hyperlink::ActiveModel,
    job_id: Option<i32>,
    mode: TagClassificationMode,
) -> Result<TagClassificationOutput, ProcessingError> {
    let settings = crate::model::tagging_settings::load(connection)
        .await
        .map_err(ProcessingError::DB)?;

    if !settings.classification_enabled() {
        return Ok(TagClassificationOutput {
            classified: false,
            skipped_reason: Some("tagging disabled".to_string()),
            tag_count: 0,
        });
    }

    let hyperlink_id = *hyperlink.id.as_ref();
    let request = LlmTaggingRequest {
        title: hyperlink.title.as_ref().to_string(),
        url: hyperlink.url.as_ref().to_string(),
        og_title: hyperlink.og_title.as_ref().clone(),
        og_description: hyperlink.og_description.as_ref().clone(),
        vocabulary: settings.vocabulary.clone(),
    };

    let llm_response = classify_tags_with_provider(&settings, &request, mode).await?;
    let normalized_tags = normalize_for_mode(llm_response.ranked_tags, &settings.vocabulary, mode);

    hyperlink_tagging::persist_llm_tags(
        connection,
        hyperlink_id,
        job_id,
        LlmPersistInput {
            ranked_tags: normalized_tags.clone(),
            overall_confidence: llm_response.overall_confidence,
            rationale: llm_response.rationale,
            provider: settings.provider.as_storage().to_string(),
            model: settings.model.clone(),
            prompt_version: hyperlink_tagging::TAGGING_PROMPT_VERSION.to_string(),
            classified_at: Utc::now().to_rfc3339(),
        },
    )
    .await
    .map_err(ProcessingError::DB)?;

    Ok(TagClassificationOutput {
        classified: true,
        skipped_reason: None,
        tag_count: normalized_tags.len(),
    })
}

fn normalize_for_mode(
    ranked_tags: Vec<RankedTag>,
    vocabulary: &[String],
    mode: TagClassificationMode,
) -> Vec<PersistedRankedTag> {
    match mode {
        TagClassificationMode::VocabularyOnly => {
            hyperlink_tagging::normalize_ranked_tags_for_vocabulary(
                ranked_tags,
                vocabulary,
                MINIMUM_TAG_CONFIDENCE,
            )
            .into_iter()
            .map(|ranked| PersistedRankedTag {
                tag: ranked.tag,
                confidence: ranked.confidence,
                state_if_new: TagState::AiApproved,
            })
            .collect()
        }
        TagClassificationMode::DiscoverWithPending => {
            hyperlink_tagging::normalize_ranked_tags_with_discovery(
                ranked_tags,
                vocabulary,
                MINIMUM_TAG_CONFIDENCE,
            )
        }
    }
}

#[derive(Clone, Debug)]
struct LlmTaggingRequest {
    title: String,
    url: String,
    og_title: Option<String>,
    og_description: Option<String>,
    vocabulary: Vec<String>,
}

#[derive(Clone, Debug)]
struct LlmTaggingResponse {
    ranked_tags: Vec<RankedTag>,
    overall_confidence: Option<f32>,
    rationale: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct ParsedLlmTaggingResponse {
    #[serde(default)]
    ranked_tags: Vec<ParsedRankedTag>,
    overall_confidence: Option<f32>,
    rationale: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ParsedRankedTag {
    tag: String,
    confidence: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChatApiKind {
    OpenAiCompatible,
    OllamaApi,
}

impl ChatApiKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "openai_compatible",
            Self::OllamaApi => "ollama_api",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ChatEndpointCandidate {
    pub(crate) url: Url,
    pub(crate) api_kind: ChatApiKind,
}

async fn classify_tags_with_provider(
    settings: &TaggingSettings,
    request: &LlmTaggingRequest,
    mode: TagClassificationMode,
) -> Result<LlmTaggingResponse, ProcessingError> {
    match settings.provider {
        TaggingProvider::OpenAiCompatible => {
            classify_tags_openai_compatible(settings, request, mode).await
        }
    }
}

async fn classify_tags_openai_compatible(
    settings: &TaggingSettings,
    request: &LlmTaggingRequest,
    mode: TagClassificationMode,
) -> Result<LlmTaggingResponse, ProcessingError> {
    let endpoints = chat_endpoint_candidates(&settings.base_url, settings.backend_kind)
        .map_err(ProcessingError::FetchError)?;
    let system_prompt = build_system_prompt(mode);
    let user_prompt = build_user_prompt(request, mode);

    let client = reqwest::Client::builder()
        .timeout(TAG_CLASSIFY_TIMEOUT)
        .build()
        .map_err(|error| {
            ProcessingError::FetchError(format!("failed to build llm client: {error}"))
        })?;

    let auth_header = build_auth_header(settings)?;
    let mut attempt_failures = Vec::new();

    for endpoint in endpoints {
        let body = build_chat_request_body(
            endpoint.api_kind,
            &settings.model,
            system_prompt,
            &user_prompt,
        );
        let endpoint_display = endpoint.url.to_string();

        let mut builder = client
            .post(endpoint.url)
            .header(CONTENT_TYPE, "application/json")
            .json(&body);
        if let Some((header_name, header_value)) = auth_header.as_ref() {
            builder = builder.header(header_name.clone(), header_value.clone());
        }

        let response = match builder.send().await {
            Ok(response) => response,
            Err(error) => {
                attempt_failures.push(format!(
                    "{} [{}] -> request failed: {}",
                    endpoint_display,
                    endpoint.api_kind.as_str(),
                    format_reqwest_transport_error(&error)
                ));
                continue;
            }
        };
        let status = response.status();
        let body_text = match response.text().await {
            Ok(body) => body,
            Err(error) => {
                attempt_failures.push(format!(
                    "{} [{}] -> failed to read response body: {error}",
                    endpoint_display,
                    endpoint.api_kind.as_str()
                ));
                continue;
            }
        };

        if !status.is_success() {
            attempt_failures.push(format!(
                "{} [{}] -> status {status}: {}",
                endpoint_display,
                endpoint.api_kind.as_str(),
                summarize_error_body(&body_text)
            ));
            continue;
        }

        let response_json: serde_json::Value = match serde_json::from_str(&body_text) {
            Ok(value) => value,
            Err(error) => {
                attempt_failures.push(format!(
                    "{} [{}] -> invalid json response: {error}",
                    endpoint_display,
                    endpoint.api_kind.as_str()
                ));
                continue;
            }
        };
        let Some(content) = extract_chat_message_content(&response_json, endpoint.api_kind) else {
            attempt_failures.push(format!(
                "{} [{}] -> response missing assistant content",
                endpoint_display,
                endpoint.api_kind.as_str()
            ));
            continue;
        };

        let parsed = match parse_ranked_tags_content(&content) {
            Ok(parsed) => parsed,
            Err(error) => {
                attempt_failures.push(format!(
                    "{} [{}] -> invalid tagging payload: {error}",
                    endpoint_display,
                    endpoint.api_kind.as_str()
                ));
                continue;
            }
        };

        return Ok(LlmTaggingResponse {
            ranked_tags: parsed
                .ranked_tags
                .into_iter()
                .map(|ranked| RankedTag {
                    tag: ranked.tag,
                    confidence: ranked.confidence,
                })
                .collect(),
            overall_confidence: parsed.overall_confidence.map(|value| value.clamp(0.0, 1.0)),
            rationale: parsed.rationale.and_then(|value| {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            }),
        });
    }

    Err(ProcessingError::FetchError(format!(
        "llm request failed across all endpoint candidates: {}",
        attempt_failures.join(" | ")
    )))
}

fn build_system_prompt(mode: TagClassificationMode) -> &'static str {
    match mode {
        TagClassificationMode::VocabularyOnly => {
            "You classify a hyperlink into ranked tags.
Return strict JSON only.
Output schema:
{
  \"ranked_tags\": [{\"tag\": \"string\", \"confidence\": 0.0}],
  \"overall_confidence\": 0.0,
  \"rationale\": \"optional short reason\"
}
Rules:
- Use only tags from the provided vocabulary.
- Rank tags from best to worst.
- Include only tags that are actually justified; zero tags is valid.
- Confidence must be between 0.0 and 1.0."
        }
        TagClassificationMode::DiscoverWithPending => {
            "You classify a hyperlink into ranked tags.
Return strict JSON only.
Output schema:
{
  \"ranked_tags\": [{\"tag\": \"string\", \"confidence\": 0.0}],
  \"overall_confidence\": 0.0,
  \"rationale\": \"optional short reason\"
}
Rules:
- Prefer tags from the provided vocabulary when they fit.
- You may propose concise new tags when none of the provided vocabulary fits well.
- Rank tags from best to worst.
- Include only tags that are actually justified; zero tags is valid.
- Confidence must be between 0.0 and 1.0."
        }
    }
}

fn build_user_prompt(request: &LlmTaggingRequest, mode: TagClassificationMode) -> String {
    let payload = json!({
        "title": request.title,
        "url": request.url,
        "og_title": request.og_title,
        "og_description": request.og_description,
        "vocabulary": request.vocabulary,
        "mode": match mode {
            TagClassificationMode::VocabularyOnly => "vocabulary_only",
            TagClassificationMode::DiscoverWithPending => "discover_with_pending",
        }
    });
    format!("Classify this hyperlink payload:\n{payload}")
}

pub(crate) fn chat_endpoint_candidates(
    base_url: &str,
    preferred_backend: TaggingBackendKind,
) -> Result<Vec<ChatEndpointCandidate>, String> {
    let base_url = base_url.trim();
    let mut parsed =
        Url::parse(base_url).map_err(|error| format!("invalid base URL `{base_url}`: {error}"))?;
    parsed.set_query(None);
    parsed.set_fragment(None);

    let base_path = parsed.path().trim_end_matches('/');
    let mut candidates = Vec::new();
    let mut seen = HashSet::<String>::new();

    if base_path.ends_with("/api/chat") {
        push_chat_candidate(
            &mut candidates,
            &mut seen,
            &parsed,
            Some(base_path.to_string()),
            ChatApiKind::OllamaApi,
        );
    }
    if base_path.ends_with("/chat/completions") {
        push_chat_candidate(
            &mut candidates,
            &mut seen,
            &parsed,
            Some(base_path.to_string()),
            ChatApiKind::OpenAiCompatible,
        );
    }
    if base_path.ends_with("/v1") {
        push_chat_candidate(
            &mut candidates,
            &mut seen,
            &parsed,
            Some(format!("{base_path}/chat/completions")),
            ChatApiKind::OpenAiCompatible,
        );
    }
    if base_path.ends_with("/api") {
        push_chat_candidate(
            &mut candidates,
            &mut seen,
            &parsed,
            replace_path_suffix(base_path, "/api", "/v1/chat/completions"),
            ChatApiKind::OpenAiCompatible,
        );
        push_chat_candidate(
            &mut candidates,
            &mut seen,
            &parsed,
            Some(format!("{base_path}/chat")),
            ChatApiKind::OllamaApi,
        );
    }
    if !base_path.is_empty()
        && base_path != "/"
        && !base_path.ends_with("/api/chat")
        && !base_path.ends_with("/chat/completions")
        && !base_path.ends_with("/v1")
        && !base_path.ends_with("/api")
    {
        push_chat_candidate(
            &mut candidates,
            &mut seen,
            &parsed,
            Some(format!("{base_path}/chat/completions")),
            ChatApiKind::OpenAiCompatible,
        );
    }

    let prioritized_fallbacks: [(ChatApiKind, &str); 3] = match preferred_backend {
        TaggingBackendKind::Ollama => [
            (ChatApiKind::OllamaApi, "/api/chat"),
            (ChatApiKind::OpenAiCompatible, "/v1/chat/completions"),
            (ChatApiKind::OpenAiCompatible, "/chat/completions"),
        ],
        _ => [
            (ChatApiKind::OpenAiCompatible, "/v1/chat/completions"),
            (ChatApiKind::OpenAiCompatible, "/chat/completions"),
            (ChatApiKind::OllamaApi, "/api/chat"),
        ],
    };
    for (api_kind, path) in prioritized_fallbacks {
        push_chat_candidate(
            &mut candidates,
            &mut seen,
            &parsed,
            Some(path.to_string()),
            api_kind,
        );
    }

    Ok(candidates)
}

fn push_chat_candidate(
    candidates: &mut Vec<ChatEndpointCandidate>,
    seen: &mut HashSet<String>,
    parsed: &Url,
    path: Option<String>,
    api_kind: ChatApiKind,
) {
    let Some(path) = path else {
        return;
    };
    let normalized = normalize_candidate_path(&path);
    let dedupe_key = format!("{}|{}", normalized, api_kind.as_str());
    if !seen.insert(dedupe_key) {
        return;
    }

    let mut url = parsed.clone();
    url.set_path(&normalized);
    candidates.push(ChatEndpointCandidate { url, api_kind });
}

fn normalize_candidate_path(path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

fn replace_path_suffix(base_path: &str, from: &str, to: &str) -> Option<String> {
    if !base_path.ends_with(from) {
        return None;
    }

    let prefix = base_path.trim_end_matches(from);
    if prefix.is_empty() {
        Some(to.to_string())
    } else {
        Some(format!("{prefix}{to}"))
    }
}

fn build_auth_header(
    settings: &TaggingSettings,
) -> Result<Option<(HeaderName, HeaderValue)>, ProcessingError> {
    let Some(api_key) = settings.api_key.as_deref() else {
        return Ok(None);
    };

    let header_name = settings
        .auth_header_name
        .as_deref()
        .unwrap_or("Authorization");
    let header_prefix = settings.auth_header_prefix.as_deref().unwrap_or("Bearer");
    let header_value = if header_prefix.trim().is_empty() {
        api_key.to_string()
    } else {
        format!("{header_prefix} {api_key}")
    };

    let header_name = HeaderName::from_bytes(header_name.as_bytes()).map_err(|error| {
        ProcessingError::FetchError(format!("invalid auth header name: {error}"))
    })?;
    let header_value = HeaderValue::from_str(&header_value).map_err(|error| {
        ProcessingError::FetchError(format!("invalid auth header value: {error}"))
    })?;

    Ok(Some((header_name, header_value)))
}

pub(crate) fn build_chat_request_body(
    api_kind: ChatApiKind,
    model: &str,
    system_prompt: &str,
    user_prompt: &str,
) -> serde_json::Value {
    match api_kind {
        ChatApiKind::OpenAiCompatible => json!({
            "model": model,
            "temperature": 0.0,
            "response_format": { "type": "json_object" },
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt}
            ]
        }),
        ChatApiKind::OllamaApi => json!({
            "model": model,
            "stream": false,
            "format": "json",
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt}
            ],
            "options": {"temperature": 0.0}
        }),
    }
}

fn summarize_error_body(body: &str) -> String {
    const MAX_CHARS: usize = 240;
    let compact = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= MAX_CHARS {
        compact
    } else {
        format!("{}...", compact.chars().take(MAX_CHARS).collect::<String>())
    }
}

fn format_reqwest_transport_error(error: &reqwest::Error) -> String {
    let mut formatted = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !cause_text.is_empty() && !formatted.contains(&cause_text) {
            formatted.push_str(" | caused by: ");
            formatted.push_str(&cause_text);
        }
        source = cause.source();
    }
    formatted
}

fn extract_chat_message_content(
    response_json: &serde_json::Value,
    api_kind: ChatApiKind,
) -> Option<String> {
    if matches!(api_kind, ChatApiKind::OllamaApi) {
        if let Some(text) = response_json
            .get("message")
            .and_then(|value| value.get("content"))
            .and_then(serde_json::Value::as_str)
        {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    let first_choice = response_json.get("choices")?.as_array()?.first()?;
    let message = first_choice.get("message")?;
    let content = message.get("content")?;

    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }

    let parts = content.as_array()?;
    let mut joined = String::new();
    for part in parts {
        if let Some(text) = part.get("text").and_then(serde_json::Value::as_str) {
            if !joined.is_empty() {
                joined.push('\n');
            }
            joined.push_str(text);
        }
    }

    if joined.trim().is_empty() {
        None
    } else {
        Some(joined)
    }
}

fn parse_ranked_tags_content(content: &str) -> Result<ParsedLlmTaggingResponse, ProcessingError> {
    if let Ok(parsed) = serde_json::from_str::<ParsedLlmTaggingResponse>(content) {
        return Ok(parsed);
    }

    let stripped = strip_markdown_code_fence(content).ok_or_else(|| {
        ProcessingError::FetchError("llm response content is not valid json".to_string())
    })?;
    serde_json::from_str::<ParsedLlmTaggingResponse>(stripped).map_err(|error| {
        ProcessingError::FetchError(format!("failed to parse llm content json: {error}"))
    })
}

fn strip_markdown_code_fence(content: &str) -> Option<&str> {
    let trimmed = content.trim();
    if !trimmed.starts_with("```") {
        return None;
    }

    let without_prefix = trimmed.trim_start_matches("```");
    let newline_index = without_prefix.find('\n')?;
    let after_language = &without_prefix[newline_index + 1..];
    let suffix_index = after_language.rfind("```")?;
    Some(after_language[..suffix_index].trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_endpoint_candidates_include_openai_v1_and_ollama_fallbacks() {
        let endpoints = chat_endpoint_candidates(
            "http://ollama.local:11434/api",
            TaggingBackendKind::OpenAiCompatible,
        )
        .expect("candidates should parse");
        let urls: Vec<String> = endpoints
            .iter()
            .map(|candidate| candidate.url.to_string())
            .collect();
        assert_eq!(urls[0], "http://ollama.local:11434/v1/chat/completions");
        assert!(urls.contains(&"http://ollama.local:11434/chat/completions".to_string()));
        assert!(urls.contains(&"http://ollama.local:11434/api/chat".to_string()));
    }

    #[test]
    fn chat_endpoint_candidates_for_root_include_non_v1_openai_fallback() {
        let endpoints = chat_endpoint_candidates(
            "https://ai.fishmt.net",
            TaggingBackendKind::OpenAiCompatible,
        )
        .expect("candidates should parse");
        let urls: Vec<String> = endpoints
            .iter()
            .map(|candidate| candidate.url.to_string())
            .collect();
        assert_eq!(
            urls,
            vec![
                "https://ai.fishmt.net/v1/chat/completions".to_string(),
                "https://ai.fishmt.net/chat/completions".to_string(),
                "https://ai.fishmt.net/api/chat".to_string(),
            ]
        );
    }

    #[test]
    fn extract_chat_message_content_reads_ollama_shape() {
        let payload = serde_json::json!({
            "message": {
                "role": "assistant",
                "content": "{\"ranked_tags\":[]}"
            }
        });
        let content = extract_chat_message_content(&payload, ChatApiKind::OllamaApi)
            .expect("content should parse");
        assert_eq!(content, "{\"ranked_tags\":[]}");
    }

    #[test]
    fn parse_ranked_tags_content_accepts_markdown_fenced_json() {
        let parsed = parse_ranked_tags_content(
            "```json\n{\"ranked_tags\":[{\"tag\":\"learn\",\"confidence\":0.9}]}\n```",
        )
        .expect("content should parse");
        assert_eq!(parsed.ranked_tags.len(), 1);
        assert_eq!(parsed.ranked_tags[0].tag, "learn");
    }
}
