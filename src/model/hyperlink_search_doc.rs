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

pub async fn clear_all_readable_text(connection: &DatabaseConnection) -> Result<u64, DbErr> {
    let backend = connection.get_database_backend();
    let result = connection
        .execute(Statement::from_string(
            backend,
            r#"
                UPDATE hyperlink_search_doc
                SET readable_text = '', updated_at = CURRENT_TIMESTAMP
                WHERE readable_text <> ''
            "#
            .to_string(),
        ))
        .await?;
    Ok(result.rows_affected())
}

pub async fn clear_readable_text_for_hyperlink(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
) -> Result<u64, DbErr> {
    let backend = connection.get_database_backend();
    let result = connection
        .execute(Statement::from_sql_and_values(
            backend,
            r#"
                UPDATE hyperlink_search_doc
                SET readable_text = '', updated_at = CURRENT_TIMESTAMP
                WHERE hyperlink_id = ? AND readable_text <> ''
            "#
            .to_string(),
            vec![Value::from(hyperlink_id)],
        ))
        .await?;
    Ok(result.rows_affected())
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
