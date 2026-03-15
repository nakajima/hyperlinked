use super::*;
use crate::test_support;

#[tokio::test]
async fn record_and_list_recent_round_trip() {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_schema(&connection).await;

    record(
        &connection,
        NewLlmInteraction {
            kind: "llm_check".to_string(),
            provider: "openai_compatible".to_string(),
            model: "gpt-4.1-mini".to_string(),
            endpoint_url: "https://example.com/v1/chat/completions".to_string(),
            api_kind: "openai_compatible".to_string(),
            request_body: "{\"messages\":[]}".to_string(),
            response_body: Some("{\"ok\":true}".to_string()),
            response_status: Some(200),
            duration_ms: Some(42),
            ..Default::default()
        },
    )
    .await
    .expect("interaction should save");

    let recent = list_recent(&connection, 10)
        .await
        .expect("recent interactions should load");
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].kind, "llm_check");
    assert_eq!(recent[0].response_status, Some(200));
    assert_eq!(recent[0].response_body.as_deref(), Some("{\"ok\":true}"));
    assert_eq!(recent[0].duration_ms, Some(42));
}

#[tokio::test]
async fn list_page_clamps_requested_page_and_reports_totals() {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_schema(&connection).await;

    for index in 0..3 {
        record(
            &connection,
            NewLlmInteraction {
                kind: format!("kind-{index}"),
                provider: "openai_compatible".to_string(),
                model: "gpt-4.1-mini".to_string(),
                endpoint_url: "https://example.com/v1/chat/completions".to_string(),
                api_kind: "openai_compatible".to_string(),
                request_body: "{}".to_string(),
                created_at: Some(
                    DateTimeUtc::from(SystemTime::UNIX_EPOCH + Duration::from_secs(index as u64))
                        .naive_utc(),
                ),
                ..Default::default()
            },
        )
        .await
        .expect("interaction should save");
    }

    let page = list_page(&connection, 99, 2)
        .await
        .expect("page should load");
    assert_eq!(page.page, 2);
    assert_eq!(page.total_pages, 2);
    assert_eq!(page.total_items, 3);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].kind, "kind-0");
}

#[tokio::test]
async fn clear_all_removes_recorded_interactions() {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_schema(&connection).await;

    for index in 0..2 {
        record(
            &connection,
            NewLlmInteraction {
                kind: format!("kind-{index}"),
                provider: "openai_compatible".to_string(),
                model: "gpt-4.1-mini".to_string(),
                endpoint_url: "https://example.com/v1/chat/completions".to_string(),
                api_kind: "openai_compatible".to_string(),
                request_body: "{}".to_string(),
                ..Default::default()
            },
        )
        .await
        .expect("interaction should save");
    }

    let cleared = clear_all(&connection)
        .await
        .expect("interactions should clear");
    assert_eq!(cleared, 2);

    let recent = list_recent(&connection, 10)
        .await
        .expect("recent interactions should load");
    assert!(recent.is_empty());
}
