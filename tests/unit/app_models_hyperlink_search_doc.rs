use super::*;
use crate::{entity::hyperlink_search_doc, test_support};

#[tokio::test]
async fn upsert_readable_text_uses_current_hyperlink_fields() {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_search_schema(&connection).await;
    test_support::execute_sql(
        &connection,
        r#"
            INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
            VALUES (1, 'Original Title', 'https://example.com/original', 'https://example.com/original', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
        "#,
    )
    .await;

    upsert_readable_text(&connection, 1, "first body")
        .await
        .expect("search doc should insert");
    test_support::execute_sql(
        &connection,
        "UPDATE hyperlink SET title = 'Renamed', url = 'https://example.com/renamed' WHERE id = 1;",
    )
    .await;
    upsert_readable_text(&connection, 1, "second body")
        .await
        .expect("search doc should update");

    let row = hyperlink_search_doc::Entity::find_by_id(1)
        .one(&connection)
        .await
        .expect("search doc should load")
        .expect("search doc should exist");
    assert_eq!(row.title, "Renamed");
    assert_eq!(row.url, "https://example.com/renamed");
    assert_eq!(row.readable_text, "second body");
}

#[tokio::test]
async fn clear_helpers_reset_readable_text() {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_search_schema(&connection).await;
    test_support::execute_sql(
        &connection,
        r#"
            INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
            VALUES
                (1, 'One', 'https://example.com/one', 'https://example.com/one', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                (2, 'Two', 'https://example.com/two', 'https://example.com/two', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
        "#,
    )
    .await;

    upsert_readable_text(&connection, 1, "first body")
        .await
        .expect("first search doc should insert");
    upsert_readable_text(&connection, 2, "second body")
        .await
        .expect("second search doc should insert");

    let affected = clear_readable_text_for_hyperlink(&connection, 1)
        .await
        .expect("single clear should succeed");
    assert_eq!(affected, 1);
    assert_eq!(
        load_readable_text_excerpt_for_hyperlink(&connection, 1, 50)
            .await
            .expect("first excerpt should load"),
        None
    );
    assert_eq!(
        load_readable_text_excerpt_for_hyperlink(&connection, 2, 50)
            .await
            .expect("second excerpt should load"),
        Some("second body".to_string())
    );

    let affected = clear_all_readable_text(&connection)
        .await
        .expect("global clear should succeed");
    assert_eq!(affected, 1);
    assert_eq!(
        load_readable_text_excerpt_for_hyperlink(&connection, 2, 50)
            .await
            .expect("second excerpt should load after clear"),
        None
    );
}

#[tokio::test]
async fn load_readable_text_excerpt_trims_and_limits() {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_search_schema(&connection).await;
    test_support::execute_sql(
        &connection,
        r#"
            INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
            VALUES (1, 'One', 'https://example.com/one', 'https://example.com/one', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
        "#,
    )
    .await;

    upsert_readable_text(&connection, 1, "  alpha beta gamma  ")
        .await
        .expect("search doc should insert");

    assert_eq!(
        load_readable_text_excerpt_for_hyperlink(&connection, 1, 5)
            .await
            .expect("excerpt should load"),
        Some("alpha".to_string())
    );
}
