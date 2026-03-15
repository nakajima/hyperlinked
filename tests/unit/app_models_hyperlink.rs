use super::*;
use crate::{entity::hyperlink_tombstone, test_support};

#[tokio::test]
async fn delete_by_id_with_tombstone_marks_deletion_once() {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_schema(&connection).await;
    test_support::execute_sql(
            &connection,
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Example', 'https://example.com', 'https://example.com', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        )
        .await;

    let deleted = delete_by_id_with_tombstone(&connection, 1)
        .await
        .expect("delete should succeed");
    assert!(deleted);

    let link = hyperlink::Entity::find_by_id(1)
        .one(&connection)
        .await
        .expect("select hyperlink should work");
    assert!(link.is_none());

    let tombstone = hyperlink_tombstone::Entity::find_by_id(1)
        .one(&connection)
        .await
        .expect("select tombstone should work");
    assert!(tombstone.is_some(), "expected tombstone row");

    let deleted_again = delete_by_id_with_tombstone(&connection, 1)
        .await
        .expect("second delete should succeed");
    assert!(!deleted_again);
}
