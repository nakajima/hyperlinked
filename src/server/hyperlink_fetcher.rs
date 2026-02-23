use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
};

use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter, Statement, Value,
};
use serde::{Deserialize, Serialize};

use crate::{
    entity::{
        hyperlink,
        hyperlink_artifact::{self, HyperlinkArtifactKind},
        hyperlink_processing_job::{self, HyperlinkProcessingJobState},
    },
    model::{
        hyperlink::ROOT_DISCOVERY_DEPTH, hyperlink_artifact as hyperlink_artifact_model,
        hyperlink_processing_job as hyperlink_job_model,
    },
};

#[derive(Clone, Debug, Default, Deserialize)]
pub struct HyperlinkFetchQuery {
    pub q: Option<String>,
}

#[derive(Clone, Debug)]
pub struct HyperlinkFetchResults {
    pub links: Vec<hyperlink::Model>,
    pub latest_jobs: HashMap<i32, hyperlink_processing_job::Model>,
    pub thumbnail_artifacts: HashMap<i32, hyperlink_artifact::Model>,
    pub dark_thumbnail_artifacts: HashMap<i32, hyperlink_artifact::Model>,
    pub match_snippets: HashMap<i32, String>,
    pub parsed_query: ParsedHyperlinkQuery,
    pub ignored_tokens: Vec<String>,
    pub free_text: String,
    pub raw_q: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ParsedHyperlinkQuery {
    pub statuses: Vec<StatusToken>,
    pub scopes: Vec<ScopeToken>,
    pub types: Vec<TypeToken>,
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

impl<'a> HyperlinkFetcher<'a> {
    pub fn new(connection: &'a DatabaseConnection, query: HyperlinkFetchQuery) -> Self {
        Self { connection, query }
    }

    pub async fn fetch(self) -> Result<HyperlinkFetchResults, sea_orm::DbErr> {
        let parsed = parse_query(self.query.q.as_deref());
        let search_terms = parse_search_terms(&parsed.free_text);
        let has_free_text = !search_terms.is_empty();
        let effective_order = parsed.parsed_query.effective_order(has_free_text);

        let mut links_query = hyperlink::Entity::find();
        match parsed.parsed_query.effective_scope() {
            ScopeSelection::RootOnly => {
                links_query =
                    links_query.filter(hyperlink::Column::DiscoveryDepth.eq(ROOT_DISCOVERY_DEPTH));
            }
            ScopeSelection::DiscoveredOnly => {
                links_query =
                    links_query.filter(hyperlink::Column::DiscoveryDepth.ne(ROOT_DISCOVERY_DEPTH));
            }
            ScopeSelection::All => {}
        }

        let mut links = links_query.all(self.connection).await?;
        let relevance_scores = if has_free_text {
            let mut scores = fts_relevance_scores(self.connection, &search_terms).await?;
            let readable_text_ids = hyperlinks_with_readable_text(self.connection).await?;
            add_title_url_fallback_matches(&mut scores, &readable_text_ids, &links, &search_terms);
            links.retain(|link| scores.contains_key(&link.id));
            Some(scores)
        } else {
            None
        };

        let hyperlink_ids = links.iter().map(|link| link.id).collect::<Vec<_>>();
        let mut latest_jobs =
            hyperlink_job_model::latest_for_hyperlinks(self.connection, &hyperlink_ids).await?;

        let type_selection = parsed.parsed_query.effective_type();
        links.retain(|link| matches_type(type_selection, &link.url));

        let status_selection = parsed.parsed_query.effective_status();
        links.retain(|link| {
            matches_status(
                &status_selection,
                latest_jobs.get(&link.id).map(|job| &job.state),
            )
        });

        sort_links(&mut links, effective_order, relevance_scores.as_ref());

        let match_snippets = if has_free_text {
            build_match_snippets(self.connection, &links, &search_terms).await?
        } else {
            HashMap::new()
        };

        let shown_ids = links.iter().map(|link| link.id).collect::<HashSet<_>>();
        latest_jobs.retain(|hyperlink_id, _| shown_ids.contains(hyperlink_id));
        let shown_hyperlink_ids = links.iter().map(|link| link.id).collect::<Vec<_>>();
        let thumbnail_artifacts = hyperlink_artifact_model::latest_for_hyperlinks_kind(
            self.connection,
            &shown_hyperlink_ids,
            HyperlinkArtifactKind::ScreenshotThumbPng,
        )
        .await?;
        let dark_thumbnail_artifacts = hyperlink_artifact_model::latest_for_hyperlinks_kind(
            self.connection,
            &shown_hyperlink_ids,
            HyperlinkArtifactKind::ScreenshotThumbDarkPng,
        )
        .await?;

        Ok(HyperlinkFetchResults {
            links,
            latest_jobs,
            thumbnail_artifacts,
            dark_thumbnail_artifacts,
            match_snippets,
            parsed_query: parsed.parsed_query,
            ignored_tokens: parsed.ignored_tokens,
            free_text: parsed.free_text,
            raw_q: parsed.raw_q,
        })
    }
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

fn matches_type(selection: TypeSelection, hyperlink_url: &str) -> bool {
    let is_pdf = url_path_looks_pdf(hyperlink_url);
    match selection {
        TypeSelection::All => true,
        TypeSelection::PdfOnly => is_pdf,
        TypeSelection::NonPdfOnly => !is_pdf,
    }
}

fn matches_status(
    selection: &StatusSelection,
    latest_job_state: Option<&HyperlinkProcessingJobState>,
) -> bool {
    let status = status_from_latest_job(latest_job_state);
    match selection {
        StatusSelection::All => true,
        StatusSelection::Selected(selected) => selected.contains(&status),
    }
}

fn status_from_latest_job(latest_job_state: Option<&HyperlinkProcessingJobState>) -> StatusToken {
    match latest_job_state {
        None => StatusToken::Idle,
        Some(HyperlinkProcessingJobState::Queued | HyperlinkProcessingJobState::Running) => {
            StatusToken::Processing
        }
        Some(HyperlinkProcessingJobState::Failed) => StatusToken::Failed,
        Some(HyperlinkProcessingJobState::Succeeded) => StatusToken::Succeeded,
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

async fn fts_relevance_scores(
    connection: &DatabaseConnection,
    search_terms: &[SearchTerm],
) -> Result<HashMap<i32, f64>, sea_orm::DbErr> {
    let Some(match_query) = build_fts_match_query(search_terms) else {
        return Ok(HashMap::new());
    };

    let backend = connection.get_database_backend();
    let rows = connection
        .query_all(Statement::from_sql_and_values(
            backend,
            r#"
            SELECT
                rowid AS hyperlink_id,
                bm25(hyperlink_search_fts) AS score
            FROM hyperlink_search_fts
            WHERE hyperlink_search_fts MATCH ?
            "#
            .to_string(),
            vec![Value::from(match_query)],
        ))
        .await?;

    let mut scores = HashMap::with_capacity(rows.len());
    for row in rows {
        let hyperlink_id: i32 = row.try_get("", "hyperlink_id")?;
        let score: f64 = row.try_get("", "score")?;
        scores.insert(hyperlink_id, score);
    }

    Ok(scores)
}

async fn hyperlinks_with_readable_text(
    connection: &DatabaseConnection,
) -> Result<HashSet<i32>, sea_orm::DbErr> {
    let backend = connection.get_database_backend();
    let rows = connection
        .query_all(Statement::from_string(
            backend,
            r#"
            SELECT hyperlink_id
            FROM hyperlink_search_doc
            WHERE LENGTH(TRIM(readable_text)) > 0
            "#
            .to_string(),
        ))
        .await?;

    let mut hyperlink_ids = HashSet::with_capacity(rows.len());
    for row in rows {
        let hyperlink_id: i32 = row.try_get("", "hyperlink_id")?;
        hyperlink_ids.insert(hyperlink_id);
    }

    Ok(hyperlink_ids)
}

fn add_title_url_fallback_matches(
    relevance_scores: &mut HashMap<i32, f64>,
    readable_text_ids: &HashSet<i32>,
    links: &[hyperlink::Model],
    search_terms: &[SearchTerm],
) {
    for link in links {
        if relevance_scores.contains_key(&link.id) || readable_text_ids.contains(&link.id) {
            continue;
        }

        if matches_title_url_terms(link, search_terms) {
            relevance_scores.insert(link.id, FALLBACK_RELEVANCE_SCORE);
        }
    }
}

fn matches_title_url_terms(link: &hyperlink::Model, search_terms: &[SearchTerm]) -> bool {
    let title = link.title.to_ascii_lowercase();
    let url = link.url.to_ascii_lowercase();
    search_terms
        .iter()
        .all(|term| matches_search_term(&title, term) || matches_search_term(&url, term))
}

fn matches_search_term(value: &str, term: &SearchTerm) -> bool {
    if term.exact {
        contains_exact_match_ascii(value, &term.value)
    } else {
        value.contains(&term.value)
    }
}

fn contains_exact_match_ascii(value: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }

    for (start, _) in value.match_indices(needle) {
        let end = start + needle.len();
        if is_word_boundary_range_ascii(value, start, end) {
            return true;
        }
    }
    false
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

fn sort_links(
    links: &mut [hyperlink::Model],
    order: OrderToken,
    relevance_scores: Option<&HashMap<i32, f64>>,
) {
    match order {
        OrderToken::Newest => links.sort_by(sort_newest),
        OrderToken::Oldest => links.sort_by(sort_oldest),
        OrderToken::MostClicked => links.sort_by(sort_most_clicked),
        OrderToken::RecentlyClicked => links.sort_by(sort_recently_clicked),
        OrderToken::Random => {
            let seed = random_seed();
            links.sort_by_key(|link| random_sort_key(seed, link.id));
        }
        OrderToken::Relevance => links.sort_by(|a, b| sort_relevance(a, b, relevance_scores)),
    }
}

fn sort_relevance(
    a: &hyperlink::Model,
    b: &hyperlink::Model,
    relevance_scores: Option<&HashMap<i32, f64>>,
) -> Ordering {
    let a_score = relevance_scores
        .and_then(|scores| scores.get(&a.id))
        .copied();
    let b_score = relevance_scores
        .and_then(|scores| scores.get(&b.id))
        .copied();

    match (a_score, b_score) {
        (Some(a_score), Some(b_score)) => a_score
            .partial_cmp(&b_score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| b.created_at.cmp(&a.created_at)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => sort_newest(a, b),
    }
    .then_with(|| b.id.cmp(&a.id))
}

fn sort_newest(a: &hyperlink::Model, b: &hyperlink::Model) -> Ordering {
    b.created_at
        .cmp(&a.created_at)
        .then_with(|| b.id.cmp(&a.id))
}

fn sort_oldest(a: &hyperlink::Model, b: &hyperlink::Model) -> Ordering {
    a.created_at
        .cmp(&b.created_at)
        .then_with(|| a.id.cmp(&b.id))
}

fn sort_most_clicked(a: &hyperlink::Model, b: &hyperlink::Model) -> Ordering {
    b.clicks_count
        .cmp(&a.clicks_count)
        .then_with(|| b.created_at.cmp(&a.created_at))
        .then_with(|| b.id.cmp(&a.id))
}

fn sort_recently_clicked(a: &hyperlink::Model, b: &hyperlink::Model) -> Ordering {
    match (&a.last_clicked_at, &b.last_clicked_at) {
        (Some(a_last), Some(b_last)) => b_last
            .cmp(a_last)
            .then_with(|| b.created_at.cmp(&a.created_at)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => b.created_at.cmp(&a.created_at),
    }
    .then_with(|| b.id.cmp(&a.id))
}

fn random_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or_default()
}

fn random_sort_key(seed: u64, hyperlink_id: i32) -> u64 {
    splitmix64(seed ^ (hyperlink_id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

fn url_path_looks_pdf(url: &str) -> bool {
    url.split(['?', '#'])
        .next()
        .is_some_and(|prefix| prefix.to_ascii_lowercase().ends_with(".pdf"))
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
            "status:failed order:random kind:all is:pdf gibberish nonsense:123",
        ));
        assert_eq!(
            parsed.raw_q,
            "status:failed order:random kind:all is:pdf gibberish nonsense:123"
        );
        assert_eq!(parsed.parsed_query.statuses, vec![StatusToken::Failed]);
        assert_eq!(parsed.parsed_query.orders, vec![OrderToken::Random]);
        assert_eq!(parsed.parsed_query.scopes, vec![ScopeToken::All]);
        assert_eq!(parsed.parsed_query.types, vec![TypeToken::Pdf]);
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
    fn matches_status_maps_missing_latest_job_to_idle() {
        let selection = StatusSelection::Selected([StatusToken::Idle].into_iter().collect());
        assert!(matches_status(&selection, None));
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
