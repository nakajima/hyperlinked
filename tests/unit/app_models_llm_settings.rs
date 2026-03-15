use super::*;
use crate::test_support;

#[tokio::test]
async fn load_defaults_when_keys_are_missing() {
    let connection = test_support::new_memory_connection().await;
    let loaded = load(&connection).await.expect("settings should load");
    assert_eq!(loaded, LlmSettings::default());
}

#[tokio::test]
async fn save_normalizes_and_round_trips() {
    let connection = test_support::new_memory_connection().await;
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

#[tokio::test]
async fn load_reads_legacy_tag_settings_when_new_keys_are_missing() {
    let connection = test_support::new_memory_connection().await;
    kv_store::set(
        &connection,
        LEGACY_KEY_BASE_URL,
        "https://legacy.example.com/v1",
    )
    .await
    .expect("legacy base URL should save");
    kv_store::set(&connection, LEGACY_KEY_MODEL, "legacy-model")
        .await
        .expect("legacy model should save");
    kv_store::set(&connection, LEGACY_KEY_BACKEND_KIND, "ollama")
        .await
        .expect("legacy backend should save");

    let loaded = load(&connection).await.expect("settings should load");
    assert_eq!(loaded.base_url, "https://legacy.example.com/v1");
    assert_eq!(loaded.model, "legacy-model");
    assert_eq!(loaded.backend_kind, LlmBackendKind::Ollama);
}
