use sea_orm::DatabaseConnection;
use serde_json::{Map, Value};
use std::path::Path;

use sea_orm::entity::prelude::DateTime;

use crate::model::{
    hyperlink::{self, HyperlinkInput, UpsertResult},
    hyperlink_processing_job::ProcessingQueueSender,
};

const ROOT_KEYS: [&str; 4] = ["links", "bookmarks", "items", "data"];
const URL_KEYS: [&str; 3] = ["url", "uri", "link"];
const TITLE_KEYS: [&str; 2] = ["title", "name"];
const CREATED_AT_KEYS: [&str; 2] = ["createdAt", "created_at"];

#[derive(Clone, Debug)]
struct ParsedRow {
    input: HyperlinkInput,
    created_at: Option<DateTime>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportFormat {
    Auto,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportFailure {
    pub row: usize,
    pub message: String,
    pub entry_json: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ImportSummary {
    pub total: usize,
    pub inserted: usize,
    pub updated: usize,
    pub failed: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ImportReport {
    pub summary: ImportSummary,
    pub failures: Vec<ImportFailure>,
}

pub async fn import_file(
    connection: &DatabaseConnection,
    path: &Path,
    format: ImportFormat,
    processing_queue: Option<&ProcessingQueueSender>,
) -> Result<ImportReport, String> {
    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(|err| format!("failed to read {}: {err}", path.display()))?;
    import_json_content(connection, &content, format, processing_queue).await
}

pub async fn import_json_content(
    connection: &DatabaseConnection,
    content: &str,
    format: ImportFormat,
    processing_queue: Option<&ProcessingQueueSender>,
) -> Result<ImportReport, String> {
    let root: Value =
        serde_json::from_str(&content).map_err(|err| format!("failed to parse json: {err}"))?;

    let rows = match format {
        ImportFormat::Auto => detect_rows_auto(&root),
    }
    .ok_or_else(|| {
        "failed to detect links in input json (expected an array, or object with links/bookmarks/items/data arrays)".to_string()
    })?;

    let mut report = ImportReport {
        summary: ImportSummary {
            total: rows.len(),
            ..Default::default()
        },
        failures: Vec::new(),
    };

    for (idx, row) in rows.into_iter().enumerate() {
        let row_num = idx + 1;

        let parsed = match parse_row(row) {
            Ok(parsed) => parsed,
            Err(message) => {
                push_failure(&mut report, row_num, message, row);
                continue;
            }
        };

        let normalized = match hyperlink::validate_and_normalize(parsed.input).await {
            Ok(input) => input,
            Err(message) => {
                push_failure(&mut report, row_num, message, row);
                continue;
            }
        };

        match hyperlink::upsert_by_url(connection, normalized, parsed.created_at, processing_queue)
            .await
        {
            Ok(UpsertResult::Inserted) => report.summary.inserted += 1,
            Ok(UpsertResult::Updated) => report.summary.updated += 1,
            Err(err) => push_failure(&mut report, row_num, format!("database error: {err}"), row),
        }
    }

    Ok(report)
}

fn push_failure(report: &mut ImportReport, row_num: usize, message: String, row: &Value) {
    report.summary.failed += 1;
    report.failures.push(ImportFailure {
        row: row_num,
        message,
        entry_json: row_json(row),
    });
}

fn detect_rows_auto(root: &Value) -> Option<Vec<&Value>> {
    if let Some(rows) = detect_collection_links(root) {
        return Some(rows);
    }

    if let Some(rows) = root.as_array() {
        return Some(rows.iter().collect());
    }

    let object = root.as_object()?;

    for key in ROOT_KEYS {
        if let Some(rows) = object.get(key).and_then(Value::as_array) {
            return Some(rows.iter().collect());
        }
    }

    for key in ROOT_KEYS {
        let Some(nested) = object.get(key).and_then(Value::as_object) else {
            continue;
        };
        for nested_key in ROOT_KEYS {
            if let Some(rows) = nested.get(nested_key).and_then(Value::as_array) {
                return Some(rows.iter().collect());
            }
        }
    }

    find_link_array(root).map(|rows| rows.iter().collect())
}

fn detect_collection_links(root: &Value) -> Option<Vec<&Value>> {
    let object = root.as_object()?;
    let collections = object.get("collections")?.as_array()?;
    let mut rows = Vec::new();
    let mut saw_links_array = false;

    for collection in collections {
        let links = collection
            .as_object()
            .and_then(|value| value.get("links"))
            .and_then(Value::as_array);
        if let Some(links) = links {
            saw_links_array = true;
            rows.extend(links.iter());
        }
    }

    if saw_links_array {
        return Some(rows);
    }

    None
}

fn find_link_array(value: &Value) -> Option<&[Value]> {
    match value {
        Value::Array(rows) => {
            if array_looks_like_links(rows) {
                return Some(rows);
            }

            for row in rows {
                if let Some(found) = find_link_array(row) {
                    return Some(found);
                }
            }

            None
        }
        Value::Object(object) => {
            for key in ROOT_KEYS {
                if let Some(found) = object.get(key).and_then(find_link_array) {
                    return Some(found);
                }
            }

            for value in object.values() {
                if let Some(found) = find_link_array(value) {
                    return Some(found);
                }
            }

            None
        }
        _ => None,
    }
}

fn array_looks_like_links(rows: &[Value]) -> bool {
    rows.iter()
        .filter_map(Value::as_object)
        .any(|object| has_any_key(object, &URL_KEYS))
}

fn parse_row(row: &Value) -> Result<ParsedRow, String> {
    let object = row
        .as_object()
        .ok_or_else(|| "row is not an object".to_string())?;

    let title = first_string_value(object, &TITLE_KEYS).unwrap_or_default();
    let url = first_string_value(object, &URL_KEYS).unwrap_or_default();
    let created_at = first_string_value(object, &CREATED_AT_KEYS)
        .and_then(|value| parse_created_at_iso8601(&value));

    Ok(ParsedRow {
        input: HyperlinkInput { title, url },
        created_at,
    })
}

fn first_string_value(object: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        object
            .get(*key)
            .and_then(Value::as_str)
            .map(ToString::to_string)
    })
}

fn has_any_key(object: &Map<String, Value>, keys: &[&str]) -> bool {
    keys.iter().any(|key| object.contains_key(*key))
}

fn row_json(row: &Value) -> String {
    serde_json::to_string_pretty(row).unwrap_or_else(|_| row.to_string())
}

fn parse_created_at_iso8601(value: &str) -> Option<DateTime> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    if let Some(stripped) = value.strip_suffix('Z') {
        return parse_naive_datetime(stripped);
    }

    parse_naive_datetime(value)
}

fn parse_naive_datetime(value: &str) -> Option<DateTime> {
    const FORMATS: [&str; 4] = [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
    ];

    for format in FORMATS {
        if let Ok(parsed) = DateTime::parse_from_str(value, format) {
            return Some(parsed);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database, DatabaseConnection, EntityTrait, Statement};

    use crate::entity::{hyperlink, hyperlink_processing_job};

    async fn initialize_schema(connection: &DatabaseConnection) {
        connection
            .execute(Statement::from_string(
                connection.get_database_backend(),
                r#"
                    CREATE TABLE hyperlink (
                        id integer NOT NULL PRIMARY KEY AUTOINCREMENT,
                        title varchar NOT NULL,
                        url varchar NOT NULL,
                        raw_url varchar NOT NULL DEFAULT '',
                        source_type varchar NOT NULL DEFAULT 'unknown',
                        og_title text NULL,
                        og_description text NULL,
                        og_type text NULL,
                        og_url text NULL,
                        og_image_url text NULL,
                        og_site_name text NULL,
                        discovery_depth integer NOT NULL DEFAULT 0,
                        clicks_count integer NOT NULL DEFAULT 0,
                        last_clicked_at datetime_text NULL,
                        processing_state varchar NOT NULL DEFAULT 'waiting',
                        processing_started_at datetime_text NULL,
                        processed_at datetime_text NULL,
                        created_at datetime_text NOT NULL,
                        updated_at datetime_text NOT NULL
                    );
                    CREATE UNIQUE INDEX idx_hyperlink_url_unique ON hyperlink(url);
                    CREATE TABLE hyperlink_processing_job (
                        id integer NOT NULL PRIMARY KEY AUTOINCREMENT,
                        hyperlink_id integer NOT NULL,
                        kind varchar NOT NULL DEFAULT 'snapshot',
                        state varchar NOT NULL,
                        error_message text NULL,
                        queued_at datetime_text NOT NULL,
                        started_at datetime_text NULL,
                        finished_at datetime_text NULL,
                        created_at datetime_text NOT NULL,
                        updated_at datetime_text NOT NULL
                    );
                "#
                .to_string(),
            ))
            .await
            .expect("schema should initialize");
    }

    async fn new_connection() -> DatabaseConnection {
        let connection = Database::connect("sqlite::memory:")
            .await
            .expect("in-memory database should initialize");
        initialize_schema(&connection).await;
        connection
    }

    async fn write_temp_file(content: &str, suffix: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "hyperlinked-linkwarden-import-{suffix}-{}.json",
            std::process::id()
        ));
        tokio::fs::write(&path, content)
            .await
            .expect("temp file should be writable");
        path
    }

    async fn import_from_json(
        connection: &DatabaseConnection,
        content: &str,
        suffix: &str,
    ) -> ImportReport {
        let processing_queue = crate::queue::ProcessingQueue::connect(connection.clone())
            .await
            .expect("processing queue should initialize");
        let path = write_temp_file(content, suffix).await;
        let report = import_file(
            connection,
            &path,
            ImportFormat::Auto,
            Some(&processing_queue),
        )
        .await
        .expect("import should complete");
        tokio::fs::remove_file(path)
            .await
            .expect("temp file should be removed");
        report
    }

    #[tokio::test]
    async fn imports_top_level_array() {
        let connection = new_connection().await;
        let report = import_from_json(
            &connection,
            r#"[
                {"title":"Example","url":"https://example.com"},
                {"name":"Rust","uri":"https://www.rust-lang.org"}
            ]"#,
            "array",
        )
        .await;

        assert_eq!(report.summary.total, 2);
        assert_eq!(report.summary.inserted, 2);
        assert_eq!(report.summary.updated, 0);
        assert_eq!(report.summary.failed, 0);

        let links = hyperlink::Entity::find()
            .all(&connection)
            .await
            .expect("links should load");
        assert_eq!(links.len(), 2);

        let jobs = hyperlink_processing_job::Entity::find()
            .all(&connection)
            .await
            .expect("jobs should load");
        assert_eq!(jobs.len(), 2);
    }

    #[tokio::test]
    async fn imports_created_at_when_present() {
        let connection = new_connection().await;
        let report = import_from_json(
            &connection,
            r#"[
                {
                    "title": "Example",
                    "url": "https://example.com",
                    "createdAt": "2025-07-25T22:41:56.384Z"
                }
            ]"#,
            "created-at",
        )
        .await;

        assert_eq!(report.summary.total, 1);
        assert_eq!(report.summary.inserted, 1);
        assert_eq!(report.summary.updated, 0);
        assert_eq!(report.summary.failed, 0);

        let imported = hyperlink::Entity::find()
            .one(&connection)
            .await
            .expect("query should succeed")
            .expect("row should exist");
        let expected = DateTime::parse_from_str("2025-07-25T22:41:56.384", "%Y-%m-%dT%H:%M:%S%.f")
            .expect("timestamp should parse");
        assert_eq!(imported.created_at, expected);
    }

    #[tokio::test]
    async fn imports_object_with_nested_data_links_array() {
        let connection = new_connection().await;
        let report = import_from_json(
            &connection,
            r#"{
                "data": {
                    "links": [
                        {"name":"Example","link":"https://example.com"}
                    ]
                }
            }"#,
            "nested",
        )
        .await;

        assert_eq!(report.summary.total, 1);
        assert_eq!(report.summary.inserted, 1);
        assert_eq!(report.summary.updated, 0);
        assert_eq!(report.summary.failed, 0);
    }

    #[tokio::test]
    async fn imports_collection_links_across_multiple_collections() {
        let connection = new_connection().await;
        let report = import_from_json(
            &connection,
            r#"{
                "aiPredefinedTags": ["Compiler", "Rust"],
                "collections": [
                    {
                        "name": "One",
                        "links": [
                            {"name": "Example", "url": "https://example.com"}
                        ]
                    },
                    {
                        "name": "Two",
                        "links": [
                            {"name": "Rust", "url": "https://www.rust-lang.org"},
                            {"name": "Missing URL"}
                        ]
                    }
                ]
            }"#,
            "collections",
        )
        .await;

        assert_eq!(report.summary.total, 3);
        assert_eq!(report.summary.inserted, 2);
        assert_eq!(report.summary.updated, 0);
        assert_eq!(report.summary.failed, 1);
        assert_eq!(report.failures.len(), 1);
    }

    #[tokio::test]
    async fn updates_existing_row_by_url() {
        let connection = new_connection().await;

        let normalized = crate::model::hyperlink::validate_and_normalize(HyperlinkInput {
            title: "Old".to_string(),
            url: "https://example.com".to_string(),
        })
        .await
        .expect("seed hyperlink should normalize");

        let inserted = crate::model::hyperlink::insert(&connection, normalized, None)
            .await
            .expect("seed row should insert");

        let report = import_from_json(
            &connection,
            r#"[
                {
                    "title":"New Title",
                    "url":"https://example.com",
                    "createdAt":"2021-06-01T12:30:45.000Z"
                }
            ]"#,
            "upsert",
        )
        .await;

        assert_eq!(report.summary.total, 1);
        assert_eq!(report.summary.inserted, 0);
        assert_eq!(report.summary.updated, 1);
        assert_eq!(report.summary.failed, 0);

        let updated = hyperlink::Entity::find_by_id(inserted.id)
            .one(&connection)
            .await
            .expect("query should succeed")
            .expect("row should exist");
        assert_eq!(updated.title, "New Title");
        let expected = DateTime::parse_from_str("2021-06-01T12:30:45.000", "%Y-%m-%dT%H:%M:%S%.f")
            .expect("timestamp should parse");
        assert_eq!(updated.created_at, expected);

        let jobs = hyperlink_processing_job::Entity::find()
            .all(&connection)
            .await
            .expect("jobs should load");
        assert_eq!(jobs.len(), 0);
    }

    #[tokio::test]
    async fn continues_after_row_errors() {
        let connection = new_connection().await;
        let report = import_from_json(
            &connection,
            r#"[
                {"title":"Valid","url":"https://example.com"},
                {"title":"Missing URL"},
                123,
                {"title":"Second Valid","url":"https://www.rust-lang.org"}
            ]"#,
            "errors",
        )
        .await;

        assert_eq!(report.summary.total, 4);
        assert_eq!(report.summary.inserted, 2);
        assert_eq!(report.summary.updated, 0);
        assert_eq!(report.summary.failed, 2);
        assert_eq!(report.failures.len(), 2);
    }
}
