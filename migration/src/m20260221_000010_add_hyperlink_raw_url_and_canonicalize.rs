use sea_orm_migration::{
    prelude::*,
    sea_orm::{ConnectionTrait, DbBackend, Statement},
};
use url::Url;

const URL_UNIQUE_INDEX: &str = "idx_hyperlink_url_unique";

const TRACKING_EXACT_PARAMS: &[&str] = &[
    "fbclid", "gclid", "dclid", "gbraid", "wbraid", "msclkid", "mc_cid", "mc_eid", "igshid",
    "yclid", "_hsenc", "_hsmi",
];
const TRACKING_PREFIX_PARAMS: &[&str] = &["utm_"];
const GLOBAL_SAFE_MEANINGFUL_PARAMS: &[&str] = &[
    "q", "query", "search", "page", "p", "sort", "order", "lang", "locale", "id", "v", "t", "list",
];

#[derive(Clone, Copy, Debug)]
struct HostRule {
    host: &'static str,
    path_prefix: Option<&'static str>,
    keep_exact: &'static [&'static str],
    keep_prefix: &'static [&'static str],
}

const HOST_RULES: &[HostRule] = &[
    HostRule {
        host: "youtube.com",
        path_prefix: Some("/watch"),
        keep_exact: &["v", "list", "t", "start", "index"],
        keep_prefix: &[],
    },
    HostRule {
        host: "youtu.be",
        path_prefix: None,
        keep_exact: &["t", "start"],
        keep_prefix: &[],
    },
];

#[derive(Clone, Debug)]
struct HyperlinkRow {
    id: i32,
    title: String,
    url: String,
    raw_url: String,
    discovery_depth: i32,
    clicks_count: i32,
    last_clicked_at: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Clone, Debug)]
struct MergedHyperlink {
    title: String,
    raw_url: String,
    discovery_depth: i32,
    clicks_count: i32,
    last_clicked_at: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let connection = manager.get_connection();

        manager
            .drop_index(
                Index::drop()
                    .name(URL_UNIQUE_INDEX)
                    .table(Hyperlink::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Hyperlink::Table)
                    .add_column(
                        ColumnDef::new(Hyperlink::RawUrl)
                            .string()
                            .not_null()
                            .default(""),
                    )
                    .to_owned(),
            )
            .await?;

        connection
            .execute_unprepared("UPDATE hyperlink SET raw_url = url WHERE raw_url = ''")
            .await?;

        canonicalize_existing_urls(connection, backend).await?;
        merge_duplicate_canonicals(connection, backend).await?;

        manager
            .create_index(
                Index::create()
                    .name(URL_UNIQUE_INDEX)
                    .table(Hyperlink::Table)
                    .col(Hyperlink::Url)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name(URL_UNIQUE_INDEX)
                    .table(Hyperlink::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Hyperlink::Table)
                    .drop_column(Hyperlink::RawUrl)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(URL_UNIQUE_INDEX)
                    .table(Hyperlink::Table)
                    .col(Hyperlink::Url)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await
    }
}

async fn canonicalize_existing_urls(
    connection: &impl ConnectionTrait,
    backend: DbBackend,
) -> Result<(), DbErr> {
    let rows = connection
        .query_all(Statement::from_string(
            backend,
            "SELECT id, url FROM hyperlink ORDER BY id".to_string(),
        ))
        .await?;

    for row in rows {
        let id: i32 = row.try_get("", "id")?;
        let url: String = row.try_get("", "url")?;
        let canonical = canonicalize_url(&url).unwrap_or(url);
        execute_sql(
            connection,
            backend,
            "UPDATE hyperlink SET url = ? WHERE id = ?",
            vec![canonical.into(), id.into()],
        )
        .await?;
    }

    Ok(())
}

async fn merge_duplicate_canonicals(
    connection: &impl ConnectionTrait,
    backend: DbBackend,
) -> Result<(), DbErr> {
    let rows = load_hyperlink_rows(connection, backend).await?;
    if rows.is_empty() {
        return Ok(());
    }

    let mut idx = 0usize;
    while idx < rows.len() {
        let canonical = rows[idx].url.clone();
        let mut end = idx + 1;
        while end < rows.len() && rows[end].url == canonical {
            end += 1;
        }

        if end - idx > 1 {
            merge_group(connection, backend, &canonical, &rows[idx..end]).await?;
        }
        idx = end;
    }

    Ok(())
}

async fn load_hyperlink_rows(
    connection: &impl ConnectionTrait,
    backend: DbBackend,
) -> Result<Vec<HyperlinkRow>, DbErr> {
    let rows = connection
        .query_all(Statement::from_string(
            backend,
            r#"
            SELECT
                id,
                title,
                url,
                raw_url,
                discovery_depth,
                clicks_count,
                last_clicked_at,
                created_at,
                updated_at
            FROM hyperlink
            ORDER BY url, id
            "#
            .to_string(),
        ))
        .await?;

    rows.into_iter()
        .map(|row| {
            Ok(HyperlinkRow {
                id: row.try_get("", "id")?,
                title: row.try_get("", "title")?,
                url: row.try_get("", "url")?,
                raw_url: row.try_get("", "raw_url")?,
                discovery_depth: row.try_get("", "discovery_depth")?,
                clicks_count: row.try_get("", "clicks_count")?,
                last_clicked_at: row.try_get("", "last_clicked_at")?,
                created_at: row.try_get("", "created_at")?,
                updated_at: row.try_get("", "updated_at")?,
            })
        })
        .collect::<Result<Vec<_>, DbErr>>()
}

async fn merge_group(
    connection: &impl ConnectionTrait,
    backend: DbBackend,
    canonical_url: &str,
    rows: &[HyperlinkRow],
) -> Result<(), DbErr> {
    let survivor_id = rows[0].id;
    let merged = merged_hyperlink(rows);

    for loser in rows.iter().skip(1) {
        move_jobs_to_survivor(connection, backend, loser.id, survivor_id).await?;
        move_artifacts_to_survivor(connection, backend, loser.id, survivor_id).await?;
        move_relations_to_survivor(connection, backend, loser.id, survivor_id).await?;
        execute_sql(
            connection,
            backend,
            "DELETE FROM hyperlink WHERE id = ?",
            vec![loser.id.into()],
        )
        .await?;
    }

    execute_sql(
        connection,
        backend,
        r#"
        UPDATE hyperlink
        SET
            title = ?,
            url = ?,
            raw_url = ?,
            discovery_depth = ?,
            clicks_count = ?,
            last_clicked_at = ?,
            created_at = ?,
            updated_at = ?
        WHERE id = ?
        "#,
        vec![
            merged.title.into(),
            canonical_url.to_string().into(),
            merged.raw_url.into(),
            merged.discovery_depth.into(),
            merged.clicks_count.into(),
            merged.last_clicked_at.into(),
            merged.created_at.into(),
            merged.updated_at.into(),
            survivor_id.into(),
        ],
    )
    .await
}

async fn move_jobs_to_survivor(
    connection: &impl ConnectionTrait,
    backend: DbBackend,
    loser_id: i32,
    survivor_id: i32,
) -> Result<(), DbErr> {
    execute_sql(
        connection,
        backend,
        "UPDATE hyperlink_processing_job SET hyperlink_id = ? WHERE hyperlink_id = ?",
        vec![survivor_id.into(), loser_id.into()],
    )
    .await
}

async fn move_artifacts_to_survivor(
    connection: &impl ConnectionTrait,
    backend: DbBackend,
    loser_id: i32,
    survivor_id: i32,
) -> Result<(), DbErr> {
    execute_sql(
        connection,
        backend,
        "UPDATE hyperlink_artifact SET hyperlink_id = ? WHERE hyperlink_id = ?",
        vec![survivor_id.into(), loser_id.into()],
    )
    .await
}

async fn move_relations_to_survivor(
    connection: &impl ConnectionTrait,
    backend: DbBackend,
    loser_id: i32,
    survivor_id: i32,
) -> Result<(), DbErr> {
    execute_sql(
        connection,
        backend,
        r#"
        INSERT OR IGNORE INTO hyperlink_relation (parent_hyperlink_id, child_hyperlink_id, created_at)
        SELECT ?, child_hyperlink_id, created_at
        FROM hyperlink_relation
        WHERE parent_hyperlink_id = ?
          AND child_hyperlink_id != ?
        "#,
        vec![survivor_id.into(), loser_id.into(), survivor_id.into()],
    )
    .await?;

    execute_sql(
        connection,
        backend,
        r#"
        INSERT OR IGNORE INTO hyperlink_relation (parent_hyperlink_id, child_hyperlink_id, created_at)
        SELECT parent_hyperlink_id, ?, created_at
        FROM hyperlink_relation
        WHERE child_hyperlink_id = ?
          AND parent_hyperlink_id != ?
        "#,
        vec![survivor_id.into(), loser_id.into(), survivor_id.into()],
    )
    .await?;

    execute_sql(
        connection,
        backend,
        "DELETE FROM hyperlink_relation WHERE parent_hyperlink_id = ? OR child_hyperlink_id = ?",
        vec![loser_id.into(), loser_id.into()],
    )
    .await
}

async fn execute_sql(
    connection: &impl ConnectionTrait,
    backend: DbBackend,
    sql: &str,
    values: Vec<Value>,
) -> Result<(), DbErr> {
    connection
        .execute(Statement::from_sql_and_values(
            backend,
            sql.to_string(),
            values,
        ))
        .await?;
    Ok(())
}

fn merged_hyperlink(rows: &[HyperlinkRow]) -> MergedHyperlink {
    let survivor = &rows[0];
    let raw_url = if survivor.raw_url.trim().is_empty() {
        survivor.url.clone()
    } else {
        survivor.raw_url.clone()
    };

    let clicks_sum = rows
        .iter()
        .map(|row| row.clicks_count as i64)
        .fold(0_i64, i64::saturating_add);
    let clicks_count = clicks_sum.min(i32::MAX as i64) as i32;

    let discovery_depth = rows
        .iter()
        .map(|row| row.discovery_depth)
        .min()
        .unwrap_or(survivor.discovery_depth);
    let created_at = rows
        .iter()
        .map(|row| row.created_at.clone())
        .min()
        .unwrap_or_else(|| survivor.created_at.clone());
    let updated_at = rows
        .iter()
        .map(|row| row.updated_at.clone())
        .max()
        .unwrap_or_else(|| survivor.updated_at.clone());
    let last_clicked_at = rows
        .iter()
        .filter_map(|row| row.last_clicked_at.clone())
        .max();

    MergedHyperlink {
        title: select_merged_title(rows),
        raw_url,
        discovery_depth,
        clicks_count,
        last_clicked_at,
        created_at,
        updated_at,
    }
}

fn select_merged_title(rows: &[HyperlinkRow]) -> String {
    let mut ranked = rows.to_vec();
    ranked.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| right.id.cmp(&left.id))
    });

    for row in ranked {
        let trimmed = row.title.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == row.url.trim() {
            continue;
        }
        if trimmed == row.raw_url.trim() {
            continue;
        }
        return row.title;
    }

    rows[0].title.clone()
}

fn canonicalize_url(raw_url: &str) -> Result<String, String> {
    let raw_url = raw_url.trim();
    if raw_url.is_empty() {
        return Err("url must not be empty".to_string());
    }

    let mut url = Url::parse(raw_url).map_err(|err| format!("invalid url: {err}"))?;
    match url.scheme() {
        "http" | "https" => {}
        _ => return Err("url must use http or https".to_string()),
    }

    if (url.scheme() == "http" && url.port() == Some(80))
        || (url.scheme() == "https" && url.port() == Some(443))
    {
        url.set_port(None)
            .map_err(|_| "invalid url: failed to normalize default port".to_string())?;
    }
    url.set_fragment(None);
    if url.path().is_empty() {
        url.set_path("/");
    }

    let host = url
        .host_str()
        .ok_or_else(|| "url must include host".to_string())?
        .to_ascii_lowercase();
    let path = url.path().to_string();
    let host_rules = rules_for_host_and_path(&host, &path);
    let strict_keep_mode = !host_rules.is_empty();

    let kept_pairs = url
        .query_pairs()
        .filter(|(key, _)| !is_tracking_param(key))
        .filter(|(key, _)| should_keep_param(key, strict_keep_mode, &host_rules))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();

    url.set_query(None);
    if !kept_pairs.is_empty() {
        let mut pairs_mut = url.query_pairs_mut();
        for (key, value) in kept_pairs {
            pairs_mut.append_pair(&key, &value);
        }
        drop(pairs_mut);
    }

    Ok(format_canonical_url(&url))
}

fn rules_for_host_and_path(host: &str, path: &str) -> Vec<&'static HostRule> {
    HOST_RULES
        .iter()
        .filter(|rule| host_matches_rule(host, rule.host))
        .filter(|rule| {
            rule.path_prefix
                .is_none_or(|prefix| path.starts_with(prefix))
        })
        .collect()
}

fn host_matches_rule(host: &str, rule_host: &str) -> bool {
    host == rule_host || host.ends_with(&format!(".{rule_host}"))
}

fn should_keep_param(key: &str, strict_keep_mode: bool, host_rules: &[&HostRule]) -> bool {
    if !strict_keep_mode {
        return true;
    }

    is_exact_param_match(key, GLOBAL_SAFE_MEANINGFUL_PARAMS)
        || host_rules.iter().any(|rule| {
            is_exact_param_match(key, rule.keep_exact)
                || is_prefix_param_match(key, rule.keep_prefix)
        })
}

fn is_tracking_param(key: &str) -> bool {
    is_exact_param_match(key, TRACKING_EXACT_PARAMS)
        || is_prefix_param_match(key, TRACKING_PREFIX_PARAMS)
}

fn is_exact_param_match(key: &str, candidates: &[&str]) -> bool {
    let lowered = key.to_ascii_lowercase();
    candidates.iter().any(|candidate| lowered == *candidate)
}

fn is_prefix_param_match(key: &str, prefixes: &[&str]) -> bool {
    let lowered = key.to_ascii_lowercase();
    prefixes.iter().any(|prefix| lowered.starts_with(prefix))
}

fn format_canonical_url(url: &Url) -> String {
    let mut canonical = url.to_string();
    if url.path() == "/" {
        if url.query().is_none() {
            canonical.pop();
        } else {
            canonical = canonical.replacen("/?", "?", 1);
        }
    }
    canonical
}

#[derive(DeriveIden)]
enum Hyperlink {
    Table,
    Url,
    RawUrl,
}
