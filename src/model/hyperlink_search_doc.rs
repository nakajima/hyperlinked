use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, Statement, Value};

pub async fn upsert_readable_text(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    readable_text: &str,
) -> Result<(), DbErr> {
    let backend = connection.get_database_backend();
    connection
        .execute(Statement::from_sql_and_values(
            backend,
            r#"
            INSERT INTO hyperlink_search_doc (hyperlink_id, title, url, readable_text, updated_at)
            SELECT h.id, h.title, h.url, ?, CURRENT_TIMESTAMP
            FROM hyperlink h
            WHERE h.id = ?
            ON CONFLICT(hyperlink_id) DO UPDATE SET
                title = excluded.title,
                url = excluded.url,
                readable_text = excluded.readable_text,
                updated_at = CURRENT_TIMESTAMP
            "#
            .to_string(),
            vec![
                Value::from(readable_text.to_string()),
                Value::from(hyperlink_id),
            ],
        ))
        .await
        .map(|_| ())
}

pub fn is_search_doc_missing_error(error: &DbErr) -> bool {
    match error {
        DbErr::Exec(exec_error) => exec_error
            .to_string()
            .contains("no such table: hyperlink_search_doc"),
        DbErr::Query(query_error) => query_error
            .to_string()
            .contains("no such table: hyperlink_search_doc"),
        _ => false,
    }
}
