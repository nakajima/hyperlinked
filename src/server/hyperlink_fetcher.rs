use std::collections::{HashMap, HashSet};

use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait, QueryFilter,
    Statement, Value,
};
use serde::{Deserialize, Serialize};

use crate::{
    entity::{
        hyperlink,
        hyperlink_artifact::{self, HyperlinkArtifactKind},
        hyperlink_processing_job,
    },
    model::{
        hyperlink::ROOT_DISCOVERY_DEPTH, hyperlink_artifact as hyperlink_artifact_model,
        hyperlink_processing_job as hyperlink_job_model,
    },
};

#[derive(Clone, Debug, Default, Deserialize)]
pub struct HyperlinkFetchQuery {
    pub q: Option<String>,
    pub page: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct HyperlinkFetchResults {
    pub links: Vec<hyperlink::Model>,
    pub latest_jobs: HashMap<i32, hyperlink_processing_job::Model>,
    pub active_processing_job_ids: HashSet<i32>,
    pub thumbnail_artifacts: HashMap<i32, hyperlink_artifact::Model>,
    pub dark_thumbnail_artifacts: HashMap<i32, hyperlink_artifact::Model>,
    pub match_snippets: HashMap<i32, String>,
    pub parsed_query: ParsedHyperlinkQuery,
    pub ignored_tokens: Vec<String>,
    pub free_text: String,
    pub raw_q: String,
    pub page: u64,
    pub total_pages: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ParsedHyperlinkQuery {
    pub statuses: Vec<StatusToken>,
    pub scopes: Vec<ScopeToken>,
    pub types: Vec<TypeToken>,
    pub clicks: Vec<ClickToken>,
    pub orders: Vec<OrderToken>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusToken {
    All,
    Processing,
    Failed,
    Idle,
    Succeeded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeToken {
    Root,
    All,
    Discovered,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum TypeToken {
    #[serde(rename = "all")]
    All,
    #[serde(rename = "pdf")]
    Pdf,
    #[serde(rename = "non-pdf")]
    NonPdf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum ClickToken {
    #[serde(rename = "unclicked")]
    Unclicked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum OrderToken {
    #[serde(rename = "newest")]
    Newest,
    #[serde(rename = "oldest")]
    Oldest,
    #[serde(rename = "most-clicked")]
    MostClicked,
    #[serde(rename = "recently-clicked")]
    RecentlyClicked,
    #[serde(rename = "random")]
    Random,
    #[serde(rename = "relevance")]
    Relevance,
}

pub struct HyperlinkFetcher<'a> {
    connection: &'a DatabaseConnection,
    query: HyperlinkFetchQuery,
}

const INDEX_PER_PAGE: u64 = 100;

impl<'a> HyperlinkFetcher<'a> {
    pub fn new(connection: &'a DatabaseConnection, query: HyperlinkFetchQuery) -> Self {
        Self { connection, query }
    }

    pub async fn fetch(self) -> Result<HyperlinkFetchResults, sea_orm::DbErr> {
        let parsed = parse_query(self.query.q.as_deref());
        let search_terms = parse_search_terms(&parsed.free_text);
        let has_free_text = !search_terms.is_empty();
        let effective_order = parsed.parsed_query.effective_order(has_free_text);
        let requested_page = resolve_page(self.query.page);
        let queue_jobs_table_available = queue_jobs_table_exists(self.connection).await?;
        let page_slice = fetch_hyperlink_page_slice(
            self.connection,
            &parsed.parsed_query,
            &search_terms,
            effective_order,
            requested_page,
            INDEX_PER_PAGE,
            queue_jobs_table_available,
        )
        .await?;

        let links = load_hyperlinks_by_ids(self.connection, &page_slice.hyperlink_ids).await?;
        let hyperlink_ids = links.iter().map(|link| link.id).collect::<Vec<_>>();
        let latest_jobs =
            hyperlink_job_model::latest_for_hyperlinks(self.connection, &hyperlink_ids).await?;
        let latest_job_ids = latest_jobs.values().map(|job| job.id).collect::<Vec<_>>();
        let active_processing_job_ids = if queue_jobs_table_available {
            active_queue_processing_job_ids(self.connection, &latest_job_ids).await?
        } else {
            HashSet::new()
        };

        let match_snippets = if !search_terms.is_empty() {
            build_match_snippets(self.connection, &links, &search_terms).await?
        } else {
            HashMap::new()
        };

        let shown_hyperlink_ids = links.iter().map(|link| link.id).collect::<Vec<_>>();
        let thumbnail_artifacts = hyperlink_artifact_model::latest_for_hyperlinks_kind(
            self.connection,
            &shown_hyperlink_ids,
            HyperlinkArtifactKind::ScreenshotThumbWebp,
        )
        .await?;
        let dark_thumbnail_artifacts = hyperlink_artifact_model::latest_for_hyperlinks_kind(
            self.connection,
            &shown_hyperlink_ids,
            HyperlinkArtifactKind::ScreenshotThumbDarkWebp,
        )
        .await?;

        Ok(HyperlinkFetchResults {
            links,
            latest_jobs,
            active_processing_job_ids,
            thumbnail_artifacts,
            dark_thumbnail_artifacts,
            match_snippets,
            parsed_query: parsed.parsed_query,
            ignored_tokens: parsed.ignored_tokens,
            free_text: parsed.free_text,
            raw_q: parsed.raw_q,
            page: page_slice.page,
            total_pages: page_slice.total_pages,
        })
    }
}

fn processing_task_job_type() -> &'static str {
    std::any::type_name::<crate::queue::ProcessingTask>()
}

async fn queue_jobs_table_exists(connection: &DatabaseConnection) -> Result<bool, sea_orm::DbErr> {
    if connection.get_database_backend() != DbBackend::Sqlite {
        return Ok(true);
    }

    let row = connection
        .query_one(Statement::from_sql_and_values(
            connection.get_database_backend(),
            "SELECT COUNT(*) AS table_count FROM sqlite_master WHERE type = 'table' AND name = 'jobs'",
            Vec::new(),
        ))
        .await?;

    let count = row
        .as_ref()
        .and_then(|row| row.try_get::<i64>("", "table_count").ok())
        .unwrap_or_default();

    Ok(count > 0)
}

async fn active_queue_processing_job_ids(
    connection: &DatabaseConnection,
    processing_job_ids: &[i32],
) -> Result<HashSet<i32>, sea_orm::DbErr> {
    if processing_job_ids.is_empty() {
        return Ok(HashSet::new());
    }

    let mut sql = String::from(
        "SELECT DISTINCT CAST(json_extract(payload, '$.processing_job_id') AS INTEGER) AS processing_job_id
         FROM jobs
         WHERE job_type = ?
           AND status IN ('queued', 'processing')
           AND json_valid(payload)
           AND CAST(json_extract(payload, '$.processing_job_id') AS INTEGER) IN (",
    );
    for (index, _) in processing_job_ids.iter().enumerate() {
        if index > 0 {
            sql.push_str(", ");
        }
        sql.push('?');
    }
    sql.push(')');

    let mut values = Vec::with_capacity(1 + processing_job_ids.len());
    values.push(processing_task_job_type().into());
    for processing_job_id in processing_job_ids {
        values.push((*processing_job_id).into());
    }

    let backend = connection.get_database_backend();
    let rows = connection
        .query_all(Statement::from_sql_and_values(backend, sql, values))
        .await?;
    let mut active_ids = HashSet::with_capacity(rows.len());
    for row in rows {
        let processing_job_id: i32 = row.try_get("", "processing_job_id")?;
        active_ids.insert(processing_job_id);
    }

    Ok(active_ids)
}

fn resolve_page(page: Option<u64>) -> u64 {
    page.unwrap_or(1).max(1)
}

fn total_pages(total_rows: u64, per_page: u64) -> u64 {
    if total_rows == 0 {
        return 1;
    }

    total_rows.div_ceil(per_page.max(1))
}

fn page_offset(page: u64, per_page: u64) -> u64 {
    page.saturating_sub(1).saturating_mul(per_page.max(1))
}

struct HyperlinkPageSlice {
    hyperlink_ids: Vec<i32>,
    page: u64,
    total_pages: u64,
}

struct HyperlinkSqlParts {
    with_clause: String,
    joins_sql: String,
    where_sql: String,
    order_sql: String,
    values: Vec<Value>,
}

async fn fetch_hyperlink_page_slice(
    connection: &DatabaseConnection,
    parsed_query: &ParsedHyperlinkQuery,
    search_terms: &[SearchTerm],
    order: OrderToken,
    requested_page: u64,
    per_page: u64,
    queue_jobs_table_available: bool,
) -> Result<HyperlinkPageSlice, sea_orm::DbErr> {
    let sql_parts = build_hyperlink_sql_parts(
        parsed_query,
        search_terms,
        order,
        queue_jobs_table_available,
    );
    let backend = connection.get_database_backend();
    let select_prefix = if sql_parts.with_clause.is_empty() {
        String::new()
    } else {
        format!("{} ", sql_parts.with_clause)
    };

    let count_sql = format!(
        r#"
        {select_prefix}
        SELECT COUNT(*) AS total_items
        FROM hyperlink h
        {joins}
        WHERE {where_sql}
        "#,
        joins = sql_parts.joins_sql,
        where_sql = sql_parts.where_sql,
    );
    let count_row = connection
        .query_one(Statement::from_sql_and_values(
            backend,
            count_sql,
            sql_parts.values.clone(),
        ))
        .await?;
    let total_items = count_row
        .as_ref()
        .and_then(|row| row.try_get::<i64>("", "total_items").ok())
        .and_then(|count| u64::try_from(count).ok())
        .unwrap_or_default();

    let total_pages = total_pages(total_items, per_page);
    let page = requested_page.min(total_pages.max(1));
    let offset = page_offset(page, per_page);

    let page_sql = format!(
        r#"
        {select_prefix}
        SELECT h.id AS hyperlink_id
        FROM hyperlink h
        {joins}
        WHERE {where_sql}
        ORDER BY {order_sql}
        LIMIT ? OFFSET ?
        "#,
        joins = sql_parts.joins_sql,
        where_sql = sql_parts.where_sql,
        order_sql = sql_parts.order_sql,
    );
    let mut page_values = sql_parts.values.clone();
    page_values.push(i64::try_from(per_page).unwrap_or(i64::MAX).into());
    page_values.push(i64::try_from(offset).unwrap_or(i64::MAX).into());
    let page_rows = connection
        .query_all(Statement::from_sql_and_values(
            backend,
            page_sql,
            page_values,
        ))
        .await?;

    let mut hyperlink_ids = Vec::with_capacity(page_rows.len());
    for row in page_rows {
        hyperlink_ids.push(row.try_get("", "hyperlink_id")?);
    }

    Ok(HyperlinkPageSlice {
        hyperlink_ids,
        page,
        total_pages,
    })
}

fn build_hyperlink_sql_parts(
    parsed_query: &ParsedHyperlinkQuery,
    search_terms: &[SearchTerm],
    order: OrderToken,
    queue_jobs_table_available: bool,
) -> HyperlinkSqlParts {
    let mut values = Vec::new();
    let mut joins = Vec::new();
    let mut filters = Vec::new();

    let has_free_text = !search_terms.is_empty();
    let with_clause = if has_free_text {
        let match_query = build_fts_match_query(search_terms).unwrap_or_default();
        values.push(match_query.into());
        r#"
        WITH fts_matches AS (
            SELECT rowid AS hyperlink_id, bm25(hyperlink_search_fts) AS score
            FROM hyperlink_search_fts
            WHERE hyperlink_search_fts MATCH ?
        )
        "#
        .to_string()
    } else {
        String::new()
    };

    let status_selection = parsed_query.effective_status();
    if !matches!(status_selection, StatusSelection::All) {
        joins.push(
            r#"
            LEFT JOIN hyperlink_processing_job lpj
                ON lpj.id = (
                    SELECT j.id
                    FROM hyperlink_processing_job j
                    WHERE j.hyperlink_id = h.id
                    ORDER BY j.created_at DESC, j.id DESC
                    LIMIT 1
                )
            "#
            .to_string(),
        );
    }

    if has_free_text {
        joins.push("LEFT JOIN fts_matches fts ON fts.hyperlink_id = h.id".to_string());
        joins.push("LEFT JOIN hyperlink_search_doc sd ON sd.hyperlink_id = h.id".to_string());
        let fallback_sql = build_title_url_fallback_sql(search_terms, &mut values);
        filters.push(format!(
            "(fts.hyperlink_id IS NOT NULL OR (COALESCE(TRIM(sd.readable_text), '') = '' AND {fallback_sql}))"
        ));
    }

    match parsed_query.effective_scope() {
        ScopeSelection::RootOnly => {
            filters.push(format!("h.discovery_depth = {}", ROOT_DISCOVERY_DEPTH));
        }
        ScopeSelection::DiscoveredOnly => {
            filters.push(format!("h.discovery_depth <> {}", ROOT_DISCOVERY_DEPTH));
        }
        ScopeSelection::All => {}
    }

    let is_pdf_expr =
        "(lower(h.url) LIKE '%.pdf' OR lower(h.url) LIKE '%.pdf?%' OR lower(h.url) LIKE '%.pdf#%')";
    match parsed_query.effective_type() {
        TypeSelection::All => {}
        TypeSelection::PdfOnly => filters.push(is_pdf_expr.to_string()),
        TypeSelection::NonPdfOnly => filters.push(format!("NOT {is_pdf_expr}")),
    }

    if matches!(
        parsed_query.effective_clicks(),
        ClickSelection::UnclickedOnly
    ) {
        filters.push("h.clicks_count = 0".to_string());
    }

    if let StatusSelection::Selected(statuses) = status_selection {
        let mut status_terms = Vec::new();
        if statuses.contains(&StatusToken::Idle) {
            status_terms.push("lpj.id IS NULL".to_string());
        }
        if statuses.contains(&StatusToken::Processing) {
            if queue_jobs_table_available {
                values.push(processing_task_job_type().into());
                status_terms.push(
                    "((lpj.state = 'queued' OR lpj.state = 'running')
                      AND EXISTS (
                          SELECT 1
                          FROM jobs queue_job
                          WHERE queue_job.job_type = ?
                            AND queue_job.status IN ('queued', 'processing')
                            AND json_valid(queue_job.payload)
                            AND CAST(json_extract(queue_job.payload, '$.processing_job_id') AS INTEGER) = lpj.id
                      ))"
                        .to_string(),
                );
            } else {
                status_terms.push("0 = 1".to_string());
            }
        }
        if statuses.contains(&StatusToken::Failed) {
            status_terms.push("lpj.state = 'failed'".to_string());
        }
        if statuses.contains(&StatusToken::Succeeded) {
            status_terms.push("lpj.state = 'succeeded'".to_string());
        }
        if !status_terms.is_empty() {
            filters.push(format!("({})", status_terms.join(" OR ")));
        }
    }

    let where_sql = if filters.is_empty() {
        "1 = 1".to_string()
    } else {
        filters.join(" AND ")
    };

    let order_sql = match order {
        OrderToken::Newest => "h.created_at DESC, h.id DESC".to_string(),
        OrderToken::Oldest => "h.created_at ASC, h.id ASC".to_string(),
        OrderToken::MostClicked => "h.clicks_count DESC, h.created_at DESC, h.id DESC".to_string(),
        OrderToken::RecentlyClicked => {
            "(h.last_clicked_at IS NULL) ASC, h.last_clicked_at DESC, h.created_at DESC, h.id DESC"
                .to_string()
        }
        OrderToken::Random => "random()".to_string(),
        OrderToken::Relevance if has_free_text => {
            format!(
                "COALESCE(fts.score, {FALLBACK_RELEVANCE_SCORE}) ASC, h.created_at DESC, h.id DESC"
            )
        }
        OrderToken::Relevance => "h.created_at DESC, h.id DESC".to_string(),
    };

    HyperlinkSqlParts {
        with_clause,
        joins_sql: joins.join("\n"),
        where_sql,
        order_sql,
        values,
    }
}

fn build_title_url_fallback_sql(search_terms: &[SearchTerm], values: &mut Vec<Value>) -> String {
    if search_terms.is_empty() {
        return "1 = 1".to_string();
    }

    let mut term_sql = Vec::with_capacity(search_terms.len());
    for term in search_terms {
        if term.exact {
            let title_sql =
                build_exact_word_match_sql("lower(h.title)", term.value.as_str(), values);
            let url_sql = build_exact_word_match_sql("lower(h.url)", term.value.as_str(), values);
            term_sql.push(format!("(({title_sql}) OR ({url_sql}))"));
        } else {
            let pattern = format!("%{}%", escape_like(term.value.as_str()));
            values.push(pattern.clone().into());
            values.push(pattern.into());
            term_sql.push(
                "(lower(h.title) LIKE ? ESCAPE '\\' OR lower(h.url) LIKE ? ESCAPE '\\')"
                    .to_string(),
            );
        }
    }
    term_sql.join(" AND ")
}

fn build_exact_word_match_sql(column_sql: &str, value: &str, values: &mut Vec<Value>) -> String {
    let around = format!("*[^0-9a-z]{value}[^0-9a-z]*");
    let prefix = format!("{value}[^0-9a-z]*");
    let suffix = format!("*[^0-9a-z]{value}");

    values.push(value.to_string().into());
    values.push(around.into());
    values.push(prefix.into());
    values.push(suffix.into());

    format!("{column_sql} = ? OR {column_sql} GLOB ? OR {column_sql} GLOB ? OR {column_sql} GLOB ?")
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

async fn load_hyperlinks_by_ids(
    connection: &DatabaseConnection,
    hyperlink_ids: &[i32],
) -> Result<Vec<hyperlink::Model>, sea_orm::DbErr> {
    if hyperlink_ids.is_empty() {
        return Ok(Vec::new());
    }

    let links = hyperlink::Entity::find()
        .filter(hyperlink::Column::Id.is_in(hyperlink_ids.to_vec()))
        .all(connection)
        .await?;

    let mut by_id = links
        .into_iter()
        .map(|link| (link.id, link))
        .collect::<HashMap<_, _>>();
    let mut ordered = Vec::with_capacity(hyperlink_ids.len());
    for hyperlink_id in hyperlink_ids {
        if let Some(link) = by_id.remove(hyperlink_id) {
            ordered.push(link);
        }
    }
    Ok(ordered)
}

#[derive(Clone, Copy)]
enum ScopeSelection {
    RootOnly,
    DiscoveredOnly,
    All,
}

#[derive(Clone, Copy)]
enum TypeSelection {
    All,
    PdfOnly,
    NonPdfOnly,
}

#[derive(Clone, Copy)]
enum ClickSelection {
    All,
    UnclickedOnly,
}

#[derive(Clone)]
enum StatusSelection {
    All,
    Selected(HashSet<StatusToken>),
}

impl ParsedHyperlinkQuery {
    fn effective_scope(&self) -> ScopeSelection {
        let has_all = self.scopes.contains(&ScopeToken::All);
        let has_root = self.scopes.contains(&ScopeToken::Root);
        let has_discovered = self.scopes.contains(&ScopeToken::Discovered);
        if has_all || (has_root && has_discovered) {
            ScopeSelection::All
        } else if has_discovered {
            ScopeSelection::DiscoveredOnly
        } else {
            ScopeSelection::RootOnly
        }
    }

    fn effective_type(&self) -> TypeSelection {
        let has_all = self.types.contains(&TypeToken::All);
        let has_pdf = self.types.contains(&TypeToken::Pdf);
        let has_non_pdf = self.types.contains(&TypeToken::NonPdf);
        if has_all || (has_pdf && has_non_pdf) || self.types.is_empty() {
            TypeSelection::All
        } else if has_pdf {
            TypeSelection::PdfOnly
        } else {
            TypeSelection::NonPdfOnly
        }
    }

    fn effective_clicks(&self) -> ClickSelection {
        if self.clicks.contains(&ClickToken::Unclicked) {
            ClickSelection::UnclickedOnly
        } else {
            ClickSelection::All
        }
    }

    fn effective_status(&self) -> StatusSelection {
        if self.statuses.is_empty() || self.statuses.contains(&StatusToken::All) {
            return StatusSelection::All;
        }
        StatusSelection::Selected(self.statuses.iter().copied().collect())
    }

    fn effective_order(&self, has_free_text: bool) -> OrderToken {
        match self.orders.last().copied() {
            Some(OrderToken::Relevance) if !has_free_text => OrderToken::Newest,
            Some(order) => order,
            None if has_free_text => OrderToken::Relevance,
            None => OrderToken::Newest,
        }
    }
}

const FALLBACK_RELEVANCE_SCORE: f64 = 1_000_000_000.0;

#[derive(Clone, Debug, Eq, PartialEq)]
struct SearchTerm {
    value: String,
    exact: bool,
}

fn parse_search_terms(free_text: &str) -> Vec<SearchTerm> {
    let mut terms = Vec::new();
    let mut fragment = String::new();
    let mut in_quotes = false;

    for ch in free_text.chars() {
        if ch == '"' {
            if in_quotes {
                push_exact_search_term(&mut terms, &fragment);
            } else {
                push_unquoted_search_terms(&mut terms, &fragment);
            }
            fragment.clear();
            in_quotes = !in_quotes;
            continue;
        }
        fragment.push(ch);
    }

    push_unquoted_search_terms(&mut terms, &fragment);
    terms
}

fn push_unquoted_search_terms(terms: &mut Vec<SearchTerm>, fragment: &str) {
    for token in fragment.split_whitespace() {
        if let Some(value) = normalize_unquoted_search_term(token) {
            terms.push(SearchTerm {
                value,
                exact: false,
            });
        }
    }
}

fn push_exact_search_term(terms: &mut Vec<SearchTerm>, fragment: &str) {
    if let Some(value) = normalize_exact_search_term(fragment) {
        terms.push(SearchTerm { value, exact: true });
    }
}

fn normalize_unquoted_search_term(token: &str) -> Option<String> {
    let normalized = token
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect::<String>();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn normalize_exact_search_term(fragment: &str) -> Option<String> {
    let mut normalized = String::new();
    let mut pending_space = false;

    for ch in fragment.chars() {
        if ch.is_alphanumeric() {
            if pending_space && !normalized.is_empty() {
                normalized.push(' ');
            }
            pending_space = false;
            normalized.extend(ch.to_lowercase());
        } else if ch.is_whitespace() || matches!(ch, '-' | '_' | '/' | '\\') {
            pending_space = true;
        }
    }

    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn is_word_boundary_range_ascii(value: &str, start: usize, end: usize) -> bool {
    is_left_word_boundary_ascii(value, start) && is_right_word_boundary_ascii(value, end)
}

fn is_left_word_boundary_ascii(value: &str, start: usize) -> bool {
    if start == 0 {
        return true;
    }
    value[..start]
        .chars()
        .next_back()
        .map(|ch| !ch.is_alphanumeric())
        .unwrap_or(true)
}

fn is_right_word_boundary_ascii(value: &str, end: usize) -> bool {
    if end >= value.len() {
        return true;
    }
    value[end..]
        .chars()
        .next()
        .map(|ch| !ch.is_alphanumeric())
        .unwrap_or(true)
}

fn build_fts_match_query(search_terms: &[SearchTerm]) -> Option<String> {
    if search_terms.is_empty() {
        return None;
    }

    Some(
        search_terms
            .iter()
            .map(|term| format!("\"{}\"", term.value.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND "),
    )
}

struct SearchDocumentRow {
    title: String,
    url: String,
    readable_text: String,
}

async fn build_match_snippets(
    connection: &DatabaseConnection,
    links: &[hyperlink::Model],
    search_terms: &[SearchTerm],
) -> Result<HashMap<i32, String>, sea_orm::DbErr> {
    let hyperlink_ids = links.iter().map(|link| link.id).collect::<Vec<_>>();
    let documents = load_search_documents(connection, &hyperlink_ids).await?;

    let mut snippets = HashMap::new();
    for link in links {
        let document = documents.get(&link.id);
        let readable_text = document
            .map(|doc| doc.readable_text.as_str())
            .unwrap_or_default();
        let title = document
            .map(|doc| doc.title.as_str())
            .unwrap_or(link.title.as_str());
        let url = document
            .map(|doc| doc.url.as_str())
            .unwrap_or(link.url.as_str());

        let readable_plain_text = markdown_to_plain_text(readable_text);
        if let Some(snippet) =
            build_match_snippet_html_from_text(&readable_plain_text, search_terms)
        {
            snippets.insert(link.id, snippet);
            continue;
        }

        let title_text = collapse_whitespace(title);
        if let Some(snippet) = highlight_plain_text_html(&title_text, search_terms) {
            snippets.insert(link.id, snippet);
            continue;
        }

        if let Some(snippet) = highlight_plain_text_html(url, search_terms) {
            snippets.insert(link.id, snippet);
        }
    }

    Ok(snippets)
}

async fn load_search_documents(
    connection: &DatabaseConnection,
    hyperlink_ids: &[i32],
) -> Result<HashMap<i32, SearchDocumentRow>, sea_orm::DbErr> {
    if hyperlink_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let placeholders = std::iter::repeat_n("?", hyperlink_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        r#"
        SELECT hyperlink_id, title, url, readable_text
        FROM hyperlink_search_doc
        WHERE hyperlink_id IN ({placeholders})
        "#
    );

    let values = hyperlink_ids
        .iter()
        .copied()
        .map(Value::from)
        .collect::<Vec<_>>();
    let rows = connection
        .query_all(Statement::from_sql_and_values(
            connection.get_database_backend(),
            sql,
            values,
        ))
        .await?;

    let mut docs = HashMap::with_capacity(rows.len());
    for row in rows {
        let hyperlink_id: i32 = row.try_get("", "hyperlink_id")?;
        let title: String = row.try_get("", "title")?;
        let url: String = row.try_get("", "url")?;
        let readable_text: String = row.try_get("", "readable_text")?;
        docs.insert(
            hyperlink_id,
            SearchDocumentRow {
                title,
                url,
                readable_text,
            },
        );
    }

    Ok(docs)
}

fn build_match_snippet_html_from_text(text: &str, search_terms: &[SearchTerm]) -> Option<String> {
    let collapsed = collapse_whitespace(text);
    if collapsed.is_empty() {
        return None;
    }

    let (match_start, match_len) = first_match_range_ascii(&collapsed, search_terms)?;
    let context = 72usize;
    let from = floor_char_boundary(&collapsed, match_start.saturating_sub(context));
    let to = ceil_char_boundary(
        &collapsed,
        (match_start + match_len + context).min(collapsed.len()),
    );

    let mut snippet = collapsed[from..to].trim().to_string();
    if from > 0 {
        snippet = format!("...{snippet}");
    }
    if to < collapsed.len() {
        snippet.push_str("...");
    }

    highlight_plain_text_html(&snippet, search_terms)
}

fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut cursor = index.min(text.len());
    while cursor > 0 && !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    cursor
}

fn ceil_char_boundary(text: &str, index: usize) -> usize {
    let mut cursor = index.min(text.len());
    while cursor < text.len() && !text.is_char_boundary(cursor) {
        cursor += 1;
    }
    cursor
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn markdown_to_plain_text(markdown: &str) -> String {
    let links_replaced = replace_markdown_links(markdown);

    let stripped_lines = links_replaced
        .lines()
        .map(strip_markdown_line_prefix)
        .collect::<Vec<_>>()
        .join(" ");

    let inline_cleaned = stripped_lines
        .replace("```", " ")
        .replace('`', "")
        .replace("**", "")
        .replace("__", "")
        .replace("~~", "")
        .replace('*', "");

    collapse_whitespace(&inline_cleaned)
}

fn replace_markdown_links(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0usize;

    while let Some(start_rel) = input[cursor..].find('[') {
        let start = cursor + start_rel;
        output.push_str(&input[cursor..start]);

        let label_start = start + 1;
        let Some(label_end_rel) = input[label_start..].find(']') else {
            output.push_str(&input[start..]);
            return output;
        };
        let label_end = label_start + label_end_rel;

        if label_end + 1 >= input.len() || input.as_bytes()[label_end + 1] != b'(' {
            output.push('[');
            cursor = label_start;
            continue;
        }

        let destination_start = label_end + 2;
        let Some(destination_end_rel) = input[destination_start..].find(')') else {
            output.push_str(&input[start..]);
            return output;
        };
        let destination_end = destination_start + destination_end_rel;

        if start > 0 && input.as_bytes()[start - 1] == b'!' {
            output.pop();
        }

        output.push_str(&input[label_start..label_end]);
        cursor = destination_end + 1;
    }

    output.push_str(&input[cursor..]);
    output
}

fn strip_markdown_line_prefix(line: &str) -> &str {
    let mut value = line.trim_start();
    while let Some(rest) = value.strip_prefix('>') {
        value = rest.trim_start();
    }

    for marker in [
        "# ", "## ", "### ", "#### ", "##### ", "###### ", "- ", "* ", "+ ",
    ] {
        if let Some(rest) = value.strip_prefix(marker) {
            return rest.trim_start();
        }
    }

    let mut digit_count = 0usize;
    for ch in value.chars() {
        if ch.is_ascii_digit() {
            digit_count += ch.len_utf8();
        } else {
            break;
        }
    }
    if digit_count > 0 {
        let remainder = &value[digit_count..];
        if let Some(rest) = remainder.strip_prefix(". ") {
            return rest.trim_start();
        }
    }

    value
}

fn first_match_range_ascii(value: &str, search_terms: &[SearchTerm]) -> Option<(usize, usize)> {
    let normalized = value.to_ascii_lowercase();
    let mut first_match = None;
    for term in search_terms {
        if term.value.is_empty() {
            continue;
        }
        for (start, _) in normalized.match_indices(&term.value) {
            let end = start + term.value.len();
            if term.exact && !is_word_boundary_range_ascii(&normalized, start, end) {
                continue;
            }
            first_match = match first_match {
                Some((best_index, best_len)) if best_index <= start => Some((best_index, best_len)),
                _ => Some((start, term.value.len())),
            };
            break;
        }
    }
    first_match
}

fn highlight_plain_text_html(value: &str, search_terms: &[SearchTerm]) -> Option<String> {
    let ranges = match_ranges_ascii(value, search_terms);
    if ranges.is_empty() {
        return None;
    }

    let mut output = String::new();
    let mut cursor = 0usize;
    for (start, end) in ranges {
        output.push_str(&escape_html(&value[cursor..start]));
        output.push_str("<em>");
        output.push_str(&escape_html(&value[start..end]));
        output.push_str("</em>");
        cursor = end;
    }
    output.push_str(&escape_html(&value[cursor..]));

    Some(output)
}

fn match_ranges_ascii(value: &str, search_terms: &[SearchTerm]) -> Vec<(usize, usize)> {
    let normalized = value.to_ascii_lowercase();
    let mut ranges = Vec::new();

    for term in search_terms {
        if term.value.is_empty() {
            continue;
        }

        let mut cursor = 0usize;
        while let Some(relative_index) = normalized[cursor..].find(&term.value) {
            let start = cursor + relative_index;
            let end = start + term.value.len();
            if term.exact && !is_word_boundary_range_ascii(&normalized, start, end) {
                cursor = end;
                continue;
            }
            ranges.push((start, end));
            cursor = end;
        }
    }

    ranges.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    let mut merged = Vec::new();
    for (start, end) in ranges {
        if let Some((_, current_end)) = merged.last_mut()
            && start <= *current_end
        {
            if end > *current_end {
                *current_end = end;
            }
            continue;
        }
        merged.push((start, end));
    }

    merged
}

fn escape_html(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#39;"),
            _ => output.push(ch),
        }
    }
    output
}

struct ParseQueryOutput {
    parsed_query: ParsedHyperlinkQuery,
    ignored_tokens: Vec<String>,
    free_text: String,
    raw_q: String,
}

fn parse_query(raw_q: Option<&str>) -> ParseQueryOutput {
    let raw_q = raw_q.unwrap_or_default().trim().to_string();
    let mut parsed_query = ParsedHyperlinkQuery::default();
    let mut ignored_tokens = Vec::new();
    let mut free_text = Vec::new();

    for token in raw_q.split_whitespace() {
        let Some((raw_key, raw_value)) = token.split_once(':') else {
            free_text.push(token.to_string());
            continue;
        };

        if raw_key.is_empty() || raw_value.is_empty() {
            ignored_tokens.push(token.to_string());
            continue;
        }

        let key = normalize_key(raw_key);
        let value = raw_value.to_ascii_lowercase().replace('_', "-");
        let valid = match key.as_str() {
            "status" => parse_status_token(&value)
                .map(|status| push_unique(&mut parsed_query.statuses, status))
                .is_some(),
            "scope" => parse_scope_token(&value)
                .map(|scope| push_unique(&mut parsed_query.scopes, scope))
                .is_some(),
            "with" => parse_with_token(&value)
                .map(|scope| push_unique(&mut parsed_query.scopes, scope))
                .is_some(),
            "type" => parse_type_token(&value)
                .map(|link_type| push_unique(&mut parsed_query.types, link_type))
                .is_some(),
            "clicks" => parse_click_token(&value)
                .map(|clicks| push_unique(&mut parsed_query.clicks, clicks))
                .is_some(),
            "order" => parse_order_token(&value)
                .map(|order| parsed_query.orders.push(order))
                .is_some(),
            _ => false,
        };

        if !valid {
            ignored_tokens.push(token.to_string());
        }
    }

    ParseQueryOutput {
        parsed_query,
        ignored_tokens,
        free_text: free_text.join(" "),
        raw_q,
    }
}

fn normalize_key(raw_key: &str) -> String {
    match raw_key.to_ascii_lowercase().as_str() {
        "kind" => "scope".to_string(),
        "is" => "type".to_string(),
        key => key.to_string(),
    }
}

fn push_unique<T: Eq>(items: &mut Vec<T>, item: T) {
    if !items.contains(&item) {
        items.push(item);
    }
}

fn parse_status_token(value: &str) -> Option<StatusToken> {
    match value {
        "all" => Some(StatusToken::All),
        "processing" | "queued" | "running" => Some(StatusToken::Processing),
        "failed" | "error" => Some(StatusToken::Failed),
        "idle" => Some(StatusToken::Idle),
        "succeeded" | "success" => Some(StatusToken::Succeeded),
        _ => None,
    }
}

fn parse_scope_token(value: &str) -> Option<ScopeToken> {
    match value {
        "root" => Some(ScopeToken::Root),
        "all" => Some(ScopeToken::All),
        "discovered" | "sublinks" => Some(ScopeToken::Discovered),
        _ => None,
    }
}

fn parse_with_token(value: &str) -> Option<ScopeToken> {
    match value {
        "discovered" => Some(ScopeToken::All),
        _ => None,
    }
}

fn parse_type_token(value: &str) -> Option<TypeToken> {
    match value {
        "all" => Some(TypeToken::All),
        "pdf" => Some(TypeToken::Pdf),
        "non-pdf" | "nonpdf" => Some(TypeToken::NonPdf),
        _ => None,
    }
}

fn parse_click_token(value: &str) -> Option<ClickToken> {
    match value {
        "unclicked" | "zero" | "none" => Some(ClickToken::Unclicked),
        _ => None,
    }
}

fn parse_order_token(value: &str) -> Option<OrderToken> {
    match value {
        "newest" | "new" => Some(OrderToken::Newest),
        "oldest" | "old" => Some(OrderToken::Oldest),
        "most-clicked" | "top" => Some(OrderToken::MostClicked),
        "recently-clicked" | "recent" => Some(OrderToken::RecentlyClicked),
        "random" => Some(OrderToken::Random),
        "relevance" | "rank" => Some(OrderToken::Relevance),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fuzzy(value: &str) -> SearchTerm {
        SearchTerm {
            value: value.to_string(),
            exact: false,
        }
    }

    fn exact(value: &str) -> SearchTerm {
        SearchTerm {
            value: value.to_string(),
            exact: true,
        }
    }

    #[test]
    fn parse_query_collects_tokens_free_text_and_ignored_tokens() {
        let parsed = parse_query(Some(
            "status:failed order:random kind:all is:pdf clicks:unclicked gibberish nonsense:123",
        ));
        assert_eq!(
            parsed.raw_q,
            "status:failed order:random kind:all is:pdf clicks:unclicked gibberish nonsense:123"
        );
        assert_eq!(parsed.parsed_query.statuses, vec![StatusToken::Failed]);
        assert_eq!(parsed.parsed_query.orders, vec![OrderToken::Random]);
        assert_eq!(parsed.parsed_query.scopes, vec![ScopeToken::All]);
        assert_eq!(parsed.parsed_query.types, vec![TypeToken::Pdf]);
        assert_eq!(parsed.parsed_query.clicks, vec![ClickToken::Unclicked]);
        assert_eq!(parsed.free_text, "gibberish");
        assert_eq!(parsed.ignored_tokens, vec!["nonsense:123".to_string()]);
    }

    #[test]
    fn parse_query_merges_status_aliases_and_avoids_duplicates() {
        let parsed = parse_query(Some("status:running status:processing status:queued"));
        assert_eq!(parsed.parsed_query.statuses, vec![StatusToken::Processing]);
    }

    #[test]
    fn parse_query_supports_with_discovered_alias() {
        let parsed = parse_query(Some("with:discovered"));
        assert_eq!(parsed.parsed_query.scopes, vec![ScopeToken::All]);
        assert!(parsed.ignored_tokens.is_empty());
    }

    #[test]
    fn parse_query_rejects_unknown_with_tokens() {
        let parsed = parse_query(Some("with:unknown"));
        assert_eq!(parsed.ignored_tokens, vec!["with:unknown".to_string()]);
    }

    #[test]
    fn parse_query_supports_clicks_unclicked() {
        let parsed = parse_query(Some("clicks:unclicked"));
        assert_eq!(parsed.parsed_query.clicks, vec![ClickToken::Unclicked]);
    }

    #[test]
    fn parse_query_rejects_unknown_click_tokens() {
        let parsed = parse_query(Some("clicks:popular"));
        assert_eq!(parsed.parsed_query.clicks, Vec::<ClickToken>::new());
        assert_eq!(parsed.ignored_tokens, vec!["clicks:popular".to_string()]);
    }

    #[test]
    fn build_hyperlink_sql_parts_applies_unclicked_filter() {
        let mut parsed = ParsedHyperlinkQuery::default();
        parsed.clicks.push(ClickToken::Unclicked);

        let sql_parts = build_hyperlink_sql_parts(&parsed, &[], OrderToken::Newest, true);
        assert!(sql_parts.where_sql.contains("h.clicks_count = 0"));
    }

    #[test]
    fn build_hyperlink_sql_parts_skips_unclicked_filter_by_default() {
        let parsed = ParsedHyperlinkQuery::default();
        let sql_parts = build_hyperlink_sql_parts(&parsed, &[], OrderToken::Newest, true);
        assert!(!sql_parts.where_sql.contains("h.clicks_count = 0"));
    }

    #[test]
    fn build_hyperlink_sql_parts_processing_status_requires_active_queue_row() {
        let mut parsed = ParsedHyperlinkQuery::default();
        parsed.statuses.push(StatusToken::Processing);

        let sql_parts = build_hyperlink_sql_parts(&parsed, &[], OrderToken::Newest, true);
        assert!(sql_parts.where_sql.contains("EXISTS ("));
        assert!(
            sql_parts
                .where_sql
                .contains("queue_job.status IN ('queued', 'processing')")
        );
        assert_eq!(sql_parts.values.len(), 1);
    }

    #[test]
    fn build_hyperlink_sql_parts_processing_status_without_queue_table_returns_no_matches() {
        let mut parsed = ParsedHyperlinkQuery::default();
        parsed.statuses.push(StatusToken::Processing);

        let sql_parts = build_hyperlink_sql_parts(&parsed, &[], OrderToken::Newest, false);
        assert!(sql_parts.where_sql.contains("0 = 1"));
        assert!(!sql_parts.where_sql.contains("FROM jobs queue_job"));
        assert_eq!(sql_parts.values.len(), 0);
    }

    #[test]
    fn effective_scope_defaults_to_root_and_supports_all_semantics() {
        let mut parsed = ParsedHyperlinkQuery::default();
        assert!(matches!(parsed.effective_scope(), ScopeSelection::RootOnly));

        parsed.scopes.push(ScopeToken::Discovered);
        assert!(matches!(
            parsed.effective_scope(),
            ScopeSelection::DiscoveredOnly
        ));

        parsed.scopes.push(ScopeToken::Root);
        assert!(matches!(parsed.effective_scope(), ScopeSelection::All));
    }

    #[test]
    fn parse_query_keeps_last_order_token() {
        let parsed = parse_query(Some("order:newest order:random"));
        assert_eq!(
            parsed.parsed_query.effective_order(false),
            OrderToken::Random
        );
    }

    #[test]
    fn parse_query_supports_relevance_order() {
        let parsed = parse_query(Some("order:relevance"));
        assert_eq!(parsed.parsed_query.orders, vec![OrderToken::Relevance]);
    }

    #[test]
    fn effective_order_defaults_to_relevance_when_free_text_exists() {
        let parsed = ParsedHyperlinkQuery::default();
        assert_eq!(parsed.effective_order(true), OrderToken::Relevance);
    }

    #[test]
    fn effective_order_falls_back_from_relevance_without_text() {
        let mut parsed = ParsedHyperlinkQuery::default();
        parsed.orders.push(OrderToken::Relevance);
        assert_eq!(parsed.effective_order(false), OrderToken::Newest);
    }

    #[test]
    fn parse_search_terms_supports_quoted_exact_terms() {
        let terms = parse_search_terms(r#"parser "talk talk" "rust""#);
        assert_eq!(
            terms,
            vec![fuzzy("parser"), exact("talk talk"), exact("rust")]
        );
    }

    #[test]
    fn snippet_builder_extracts_context_for_matching_term() {
        let snippet = build_match_snippet_html_from_text(
            "one two three four rust five six seven",
            &[fuzzy("rust")],
        );
        assert_eq!(
            snippet.as_deref(),
            Some("one two three four <em>rust</em> five six seven")
        );
    }

    #[test]
    fn snippet_builder_returns_none_when_no_term_matches() {
        let snippet = build_match_snippet_html_from_text(
            "one two three four",
            &[fuzzy("rust"), fuzzy("golang")],
        );
        assert!(snippet.is_none());
    }

    #[test]
    fn exact_term_highlighting_uses_word_boundaries() {
        let snippet = highlight_plain_text_html("parsers parser", &[exact("parser")]);
        assert_eq!(snippet.as_deref(), Some("parsers <em>parser</em>"));
    }

    #[test]
    fn markdown_to_plain_text_strips_link_markup() {
        let plain = markdown_to_plain_text("Read [parser docs](https://example.com/docs) now");
        assert_eq!(plain, "Read parser docs now");
    }
}
