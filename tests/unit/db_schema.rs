use super::*;
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

#[tokio::test]
async fn ensure_current_is_idempotent() {
    let connection = Database::connect(crate::db::MEMORY)
        .await
        .expect("in-memory database should connect");

    sync(&connection)
        .await
        .expect("first schema sync should succeed");
    sync(&connection)
        .await
        .expect("second schema sync should succeed");
}

#[tokio::test]
async fn sync_replaces_legacy_manual_unique_indexes_with_entity_managed_indexes() {
    let connection = Database::connect(crate::db::MEMORY)
        .await
        .expect("in-memory database should connect");

    sync(&connection)
        .await
        .expect("initial schema sync should succeed");

    for statement in [
        r#"DROP INDEX IF EXISTS "idx-hyperlink_artifact-job_kind""#,
        r#"DROP INDEX IF EXISTS "idx-hyperlink_relation-parent_child""#,
        r#"DROP INDEX IF EXISTS "idx-hyperlink_tag-link_tag""#,
        r#"DROP INDEX IF EXISTS "idx-hyperlink_topic_tag-link_topic_tag""#,
        r#"DROP INDEX IF EXISTS "idx-hyperlink_action_tag-link_action_tag""#,
        r#"CREATE UNIQUE INDEX IF NOT EXISTS idx_hyperlink_artifact_job_id_kind ON hyperlink_artifact (job_id, kind)"#,
        r#"CREATE UNIQUE INDEX IF NOT EXISTS idx_hyperlink_relation_parent_child_unique ON hyperlink_relation (parent_hyperlink_id, child_hyperlink_id)"#,
        r#"CREATE UNIQUE INDEX IF NOT EXISTS idx_hyperlink_tag_hyperlink_id_tag_id_unique ON hyperlink_tag (hyperlink_id, tag_id)"#,
        r#"CREATE UNIQUE INDEX IF NOT EXISTS idx_hyperlink_topic_tag_hyperlink_id_topic_tag_id_unique ON hyperlink_topic_tag (hyperlink_id, topic_tag_id)"#,
        r#"CREATE UNIQUE INDEX IF NOT EXISTS idx_hyperlink_action_tag_hyperlink_id_action_tag_id_unique ON hyperlink_action_tag (hyperlink_id, action_tag_id)"#,
    ] {
        connection
            .execute_unprepared(statement)
            .await
            .expect("legacy index setup should succeed");
    }

    sync(&connection)
        .await
        .expect("schema sync should replace legacy unique indexes");

    for index_name in [
        "idx_hyperlink_artifact_job_id_kind",
        "idx_hyperlink_relation_parent_child_unique",
        "idx_hyperlink_tag_hyperlink_id_tag_id_unique",
        "idx_hyperlink_topic_tag_hyperlink_id_topic_tag_id_unique",
        "idx_hyperlink_action_tag_hyperlink_id_action_tag_id_unique",
    ] {
        assert_eq!(
            index_count(&connection, index_name).await,
            0,
            "{index_name} should be removed"
        );
    }

    for index_name in [
        "idx-hyperlink_artifact-job_kind",
        "idx-hyperlink_relation-parent_child",
        "idx-hyperlink_tag-link_tag",
        "idx-hyperlink_topic_tag-link_topic_tag",
        "idx-hyperlink_action_tag-link_action_tag",
    ] {
        assert_eq!(
            index_count(&connection, index_name).await,
            1,
            "{index_name} should exist"
        );
    }
}

async fn index_count(connection: &sea_orm::DatabaseConnection, index_name: &str) -> i64 {
    let row = connection
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT COUNT(*) AS count FROM sqlite_master WHERE type = 'index' AND name = ?",
            vec![index_name.into()],
        ))
        .await
        .expect("index query should succeed")
        .expect("index query should return a row");

    row.try_get("", "count").expect("count should decode")
}
