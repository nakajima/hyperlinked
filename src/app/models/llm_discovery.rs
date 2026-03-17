use std::{collections::HashSet, error::Error as _};

use reqwest::Url;

use crate::app::models::llm_settings::LlmBackendKind;

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

pub(crate) fn truncate_model_discovery_body(body: &str) -> String {
    const MAX_LEN: usize = 240;
    let trimmed = body.trim();
    if trimmed.len() <= MAX_LEN {
        return trimmed.to_string();
    }

    let mut truncated = String::with_capacity(MAX_LEN + 3);
    for (index, ch) in trimmed.chars().enumerate() {
        if index >= MAX_LEN {
            break;
        }
        truncated.push(ch);
    }
    truncated.push_str("...");
    truncated
}

pub(crate) fn chat_endpoint_candidates(
    base_url: &str,
    preferred_backend: LlmBackendKind,
) -> Result<Vec<ChatEndpointCandidate>, String> {
    let (allow_openai, allow_ollama) = match preferred_backend {
        LlmBackendKind::OpenAiCompatible => (true, false),
        LlmBackendKind::Ollama => (false, true),
        LlmBackendKind::Unknown => {
            return Err(
                "llm backend kind is unknown; select or detect a backend first".to_string(),
            );
        }
    };

    let base_url = base_url.trim();
    let mut parsed =
        Url::parse(base_url).map_err(|error| format!("invalid base URL `{base_url}`: {error}"))?;
    parsed.set_query(None);
    parsed.set_fragment(None);

    let base_path = parsed.path().trim_end_matches('/');
    let mut candidates = Vec::new();
    let mut seen = HashSet::<String>::new();

    if allow_ollama && base_path.ends_with("/api/chat") {
        push_chat_candidate(
            &mut candidates,
            &mut seen,
            &parsed,
            Some(base_path.to_string()),
            ChatApiKind::OllamaApi,
        );
    }
    if allow_openai && base_path.ends_with("/chat/completions") {
        push_chat_candidate(
            &mut candidates,
            &mut seen,
            &parsed,
            Some(base_path.to_string()),
            ChatApiKind::OpenAiCompatible,
        );
    }
    if allow_openai && base_path.ends_with("/v1") {
        push_chat_candidate(
            &mut candidates,
            &mut seen,
            &parsed,
            Some(format!("{base_path}/chat/completions")),
            ChatApiKind::OpenAiCompatible,
        );
    }
    if base_path.ends_with("/api") {
        if allow_openai {
            push_chat_candidate(
                &mut candidates,
                &mut seen,
                &parsed,
                replace_path_suffix(base_path, "/api", "/v1/chat/completions"),
                ChatApiKind::OpenAiCompatible,
            );
        }
        if allow_ollama {
            push_chat_candidate(
                &mut candidates,
                &mut seen,
                &parsed,
                Some(format!("{base_path}/chat")),
                ChatApiKind::OllamaApi,
            );
        }
    }
    if allow_openai
        && !base_path.is_empty()
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

    let prioritized_fallbacks: &[(ChatApiKind, &str)] = match preferred_backend {
        LlmBackendKind::OpenAiCompatible => &[
            (ChatApiKind::OpenAiCompatible, "/v1/chat/completions"),
            (ChatApiKind::OpenAiCompatible, "/chat/completions"),
        ],
        LlmBackendKind::Ollama => &[(ChatApiKind::OllamaApi, "/api/chat")],
        LlmBackendKind::Unknown => &[],
    };
    for (api_kind, path) in prioritized_fallbacks {
        push_chat_candidate(
            &mut candidates,
            &mut seen,
            &parsed,
            Some(path.to_string()),
            *api_kind,
        );
    }

    Ok(candidates)
}

pub(crate) fn build_chat_request_body(
    api_kind: ChatApiKind,
    model: &str,
    system_prompt: &str,
    user_prompt: &str,
) -> serde_json::Value {
    match api_kind {
        ChatApiKind::OpenAiCompatible => serde_json::json!({
            "model": model,
            "temperature": 0.0,
            "response_format": { "type": "json_object" },
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt}
            ]
        }),
        ChatApiKind::OllamaApi => serde_json::json!({
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

pub(crate) fn format_reqwest_transport_error(error: &reqwest::Error) -> String {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LlmModelsApiHint {
    OpenAiCompatible,
    Ollama,
    Unknown,
}

pub(crate) fn llm_models_endpoints(base_url: &str) -> Result<Vec<Url>, String> {
    let base_url = base_url.trim();
    if base_url.is_empty() {
        return Err("base URL is empty".to_string());
    }

    let mut parsed =
        Url::parse(base_url).map_err(|error| format!("invalid base URL `{base_url}`: {error}"))?;
    parsed.set_query(None);
    parsed.set_fragment(None);

    let base_path = parsed.path().trim_end_matches('/');
    let api_hint = llm_models_api_hint(base_path);
    let mut candidates: Vec<String> = Vec::new();
    let mut seen_candidates = HashSet::new();

    push_model_endpoint_candidate(
        &mut candidates,
        &mut seen_candidates,
        replace_path_suffix(base_path, "/chat/completions", "/models"),
    );
    push_model_endpoint_candidate(
        &mut candidates,
        &mut seen_candidates,
        replace_path_suffix(base_path, "/chat/completions", "/model/info"),
    );
    if base_path.ends_with("/models") {
        push_model_endpoint_candidate(
            &mut candidates,
            &mut seen_candidates,
            Some(base_path.to_string()),
        );
        push_model_endpoint_candidate(
            &mut candidates,
            &mut seen_candidates,
            replace_path_suffix(base_path, "/models", "/model/info"),
        );
    }
    if base_path.ends_with("/model/info") {
        push_model_endpoint_candidate(
            &mut candidates,
            &mut seen_candidates,
            Some(base_path.to_string()),
        );
    }
    if base_path.ends_with("/api/tags") {
        push_model_endpoint_candidate(
            &mut candidates,
            &mut seen_candidates,
            replace_path_suffix(base_path, "/api/tags", "/v1/models"),
        );
        push_model_endpoint_candidate(
            &mut candidates,
            &mut seen_candidates,
            replace_path_suffix(base_path, "/api/tags", "/v1/model/info"),
        );
        push_model_endpoint_candidate(
            &mut candidates,
            &mut seen_candidates,
            Some(base_path.to_string()),
        );
    }
    if base_path.ends_with("/v1") {
        push_model_endpoint_candidate(
            &mut candidates,
            &mut seen_candidates,
            Some(format!("{base_path}/models")),
        );
        push_model_endpoint_candidate(
            &mut candidates,
            &mut seen_candidates,
            Some(format!("{base_path}/model/info")),
        );
    }
    if base_path.ends_with("/api") {
        push_model_endpoint_candidate(
            &mut candidates,
            &mut seen_candidates,
            replace_path_suffix(base_path, "/api", "/v1/models"),
        );
        push_model_endpoint_candidate(
            &mut candidates,
            &mut seen_candidates,
            replace_path_suffix(base_path, "/api", "/v1/model/info"),
        );
        push_model_endpoint_candidate(
            &mut candidates,
            &mut seen_candidates,
            Some(format!("{base_path}/tags")),
        );
    }
    if !base_path.is_empty()
        && base_path != "/"
        && !base_path.ends_with("/chat/completions")
        && !base_path.ends_with("/models")
        && !base_path.ends_with("/model/info")
        && !base_path.ends_with("/api/tags")
    {
        push_model_endpoint_candidate(
            &mut candidates,
            &mut seen_candidates,
            Some(format!("{base_path}/models")),
        );
        push_model_endpoint_candidate(
            &mut candidates,
            &mut seen_candidates,
            Some(format!("{base_path}/model/info")),
        );
    }

    let prioritized_fallbacks = match api_hint {
        LlmModelsApiHint::Ollama => [
            "/v1/models",
            "/v1/model/info",
            "/api/tags",
            "/models",
            "/model/info",
        ],
        _ => [
            "/v1/models",
            "/v1/model/info",
            "/models",
            "/model/info",
            "/api/tags",
        ],
    };
    for fallback in prioritized_fallbacks {
        push_model_endpoint_candidate(
            &mut candidates,
            &mut seen_candidates,
            Some(fallback.to_string()),
        );
    }

    let mut urls = Vec::with_capacity(candidates.len());
    for path in candidates {
        parsed.set_path(&path);
        urls.push(parsed.clone());
    }
    Ok(urls)
}

fn llm_models_api_hint(base_path: &str) -> LlmModelsApiHint {
    if base_path.ends_with("/api") || base_path.ends_with("/api/tags") {
        return LlmModelsApiHint::Ollama;
    }
    if base_path.ends_with("/chat/completions")
        || base_path.ends_with("/models")
        || base_path.ends_with("/model/info")
        || base_path.ends_with("/v1")
        || base_path.contains("/v1/")
    {
        return LlmModelsApiHint::OpenAiCompatible;
    }
    LlmModelsApiHint::Unknown
}

pub(crate) fn llm_backend_kind_for_models_path(path: &str) -> LlmBackendKind {
    let normalized = path.trim_end_matches('/');
    if normalized.ends_with("/api/tags") || normalized.ends_with("/api/models") {
        return LlmBackendKind::Ollama;
    }
    if normalized.ends_with("/v1/models")
        || normalized.ends_with("/models")
        || normalized.ends_with("/v1/model/info")
        || normalized.ends_with("/model/info")
    {
        return LlmBackendKind::OpenAiCompatible;
    }
    LlmBackendKind::Unknown
}

pub(crate) fn llm_backend_kind_for_chat_api(api_kind: ChatApiKind) -> LlmBackendKind {
    match api_kind {
        ChatApiKind::OpenAiCompatible => LlmBackendKind::OpenAiCompatible,
        ChatApiKind::OllamaApi => LlmBackendKind::Ollama,
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

fn push_model_endpoint_candidate(
    candidates: &mut Vec<String>,
    seen_candidates: &mut HashSet<String>,
    candidate: Option<String>,
) {
    let Some(candidate) = candidate else {
        return;
    };
    if candidate.trim().is_empty() {
        return;
    }
    let normalized = if candidate.starts_with('/') {
        candidate
    } else {
        format!("/{candidate}")
    };
    if seen_candidates.insert(normalized.clone()) {
        candidates.push(normalized);
    }
}

pub(crate) fn extract_llm_model_ids(payload: &serde_json::Value) -> Vec<String> {
    let mut models = Vec::new();

    if let Some(data) = payload.get("data") {
        collect_llm_model_ids(&mut models, data);
    }
    if let Some(data) = payload.get("models") {
        collect_llm_model_ids(&mut models, data);
    }
    if let Some(model_info) = payload.get("model_info") {
        collect_litellm_model_info_ids(&mut models, model_info);
    }
    if let Some(model_info) = payload.get("model_info_map") {
        collect_litellm_model_info_ids(&mut models, model_info);
    }

    if models.is_empty() {
        collect_llm_model_ids(&mut models, payload);
    }

    let mut deduped = HashSet::new();
    models.retain(|model| deduped.insert(model.to_ascii_lowercase()));
    models.sort_unstable();
    models
}

fn collect_llm_model_ids(models: &mut Vec<String>, value: &serde_json::Value) {
    match value {
        serde_json::Value::String(model) => push_llm_model_id(models, model),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_llm_model_ids(models, item);
            }
        }
        serde_json::Value::Object(map) => {
            if let Some(id) = map.get("id").and_then(serde_json::Value::as_str) {
                push_llm_model_id(models, id);
            }
            if let Some(model) = map.get("model").and_then(serde_json::Value::as_str) {
                push_llm_model_id(models, model);
            }
            if let Some(name) = map.get("name").and_then(serde_json::Value::as_str) {
                push_llm_model_id(models, name);
            }
            if let Some(model_name) = map.get("model_name").and_then(serde_json::Value::as_str) {
                push_llm_model_id(models, model_name);
            }
            if let Some(model_info) = map.get("model_info") {
                collect_litellm_model_info_ids(models, model_info);
            }
            if let Some(model_info) = map.get("model_info_map") {
                collect_litellm_model_info_ids(models, model_info);
            }
        }
        _ => {}
    }
}

fn collect_litellm_model_info_ids(models: &mut Vec<String>, value: &serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (candidate_id, details) in map {
                push_llm_model_id(models, candidate_id);
                collect_llm_model_ids(models, details);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_litellm_model_info_ids(models, item);
            }
        }
        _ => {}
    }
}

fn push_llm_model_id(models: &mut Vec<String>, value: &str) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }
    models.push(trimmed.to_string());
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
    let normalized = if path.starts_with('/') {
        path
    } else {
        format!("/{path}")
    };
    let dedupe_key = format!("{}|{}", normalized, api_kind.as_str());
    if !seen.insert(dedupe_key) {
        return;
    }

    let mut url = parsed.clone();
    url.set_path(&normalized);
    candidates.push(ChatEndpointCandidate { url, api_kind });
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn chat_endpoint_candidates_prioritize_expected_paths() {
        let openai = chat_endpoint_candidates(
            "https://api.openai.com/v1",
            LlmBackendKind::OpenAiCompatible,
        )
        .expect("openai candidates should build");
        let openai_urls: Vec<String> = openai
            .iter()
            .map(|candidate| candidate.url.to_string())
            .collect();
        assert_eq!(
            openai_urls,
            vec![
                "https://api.openai.com/v1/chat/completions".to_string(),
                "https://api.openai.com/chat/completions".to_string(),
            ]
        );

        let ollama = chat_endpoint_candidates("http://ollama:11434/api", LlmBackendKind::Ollama)
            .expect("ollama candidates should build");
        let ollama_urls: Vec<String> = ollama
            .iter()
            .map(|candidate| candidate.url.to_string())
            .collect();
        assert_eq!(
            ollama_urls,
            vec!["http://ollama:11434/api/chat".to_string()]
        );
    }

    #[test]
    fn build_chat_request_body_matches_backend_shape() {
        let openai = build_chat_request_body(
            ChatApiKind::OpenAiCompatible,
            "gpt-4o-mini",
            "system prompt",
            "user prompt",
        );
        assert_eq!(openai["response_format"]["type"], "json_object");
        assert_eq!(openai["messages"][0]["role"], "system");

        let ollama = build_chat_request_body(
            ChatApiKind::OllamaApi,
            "qwen3",
            "system prompt",
            "user prompt",
        );
        assert_eq!(ollama["stream"], false);
        assert_eq!(ollama["format"], "json");
        assert_eq!(ollama["options"]["temperature"], 0.0);
    }

    #[test]
    fn extract_llm_model_ids_dedupes_and_sorts_candidates() {
        let payload = json!({
            "data": [
                { "id": "gpt-4o-mini" },
                { "id": "GPT-4O-MINI" }
            ],
            "model_info": {
                "ollama/qwen3": {
                    "litellm_params": { "model": "ollama/qwen3" }
                }
            }
        });

        assert_eq!(
            extract_llm_model_ids(&payload),
            vec!["gpt-4o-mini".to_string(), "ollama/qwen3".to_string()]
        );
    }

    #[test]
    fn truncate_model_discovery_body_caps_output() {
        let truncated = truncate_model_discovery_body(&"x".repeat(300));
        assert_eq!(truncated.len(), 243);
        assert!(truncated.ends_with("..."));
    }
}
