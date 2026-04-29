use super::*;
use crate::test_support;

#[tokio::test]
async fn load_defaults_when_keys_are_missing() {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_schema(&connection).await;
    let loaded = load(&connection).await.expect("settings should load");
    assert_eq!(loaded, LlmSettings::default());
}

#[tokio::test]
async fn save_normalizes_and_round_trips() {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_schema(&connection).await;
    let saved = save(
        &connection,
        LlmSettings {
            provider: LlmProvider::OpenAiCompatible,
            base_url: "  ".to_string(),
            api_key: Some("   ".to_string()),
            model: "  custom-model  ".to_string(),
            auth_header_name: Some("   X-API-Key ".to_string()),
            auth_header_prefix: Some(" ".to_string()),
            backend_kind: LlmBackendKind::Ollama,
        },
    )
    .await
    .expect("settings should save");

    assert_eq!(saved.base_url, DEFAULT_BASE_URL);
    assert_eq!(saved.api_key, None);
    assert_eq!(saved.model, "custom-model");
    assert_eq!(saved.auth_header_name.as_deref(), Some("X-API-Key"));
    assert_eq!(
        saved.auth_header_prefix,
        Some(DEFAULT_AUTH_HEADER_PREFIX.to_string())
    );
    assert_eq!(saved.backend_kind, LlmBackendKind::Ollama);

    let loaded = load(&connection).await.expect("settings should load");
    assert_eq!(loaded, saved);
}
