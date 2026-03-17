use super::*;
use crate::test_support;

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
    let snippet =
        build_match_snippet_html_from_text("one two three four", &[fuzzy("rust"), fuzzy("golang")]);
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

#[test]
fn direct_hyperlink_query_fast_path_only_handles_simple_non_sqlite_specific_cases() {
    let parsed = ParsedHyperlinkQuery::default();
    assert!(can_use_direct_hyperlink_query(
        &parsed,
        &[],
        OrderToken::Newest
    ));
    assert!(can_use_direct_hyperlink_query(
        &parsed,
        &[],
        OrderToken::MostClicked
    ));
    assert!(!can_use_direct_hyperlink_query(
        &parsed,
        &[fuzzy("rust")],
        OrderToken::Newest
    ));
    assert!(!can_use_direct_hyperlink_query(
        &parsed,
        &[],
        OrderToken::Random
    ));

    let mut with_status = ParsedHyperlinkQuery::default();
    with_status.statuses.push(StatusToken::Failed);
    assert!(!can_use_direct_hyperlink_query(
        &with_status,
        &[],
        OrderToken::Newest
    ));
}

#[tokio::test]
async fn fetch_hyperlink_page_slice_uses_direct_query_filters_for_simple_cases() {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_schema(&connection).await;
    test_support::execute_sql(
        &connection,
        r#"
            INSERT INTO hyperlink (id, title, url, raw_url, source_type, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
            VALUES
                (1, 'Newest Root Pdf', 'https://example.com/pdf', 'https://example.com/pdf', 'pdf', 0, 0, NULL, '2026-02-19 00:00:03', '2026-02-19 00:00:03'),
                (2, 'Newest Root Html', 'https://example.com/html-new', 'https://example.com/html-new', 'html', 0, 0, NULL, '2026-02-19 00:00:02', '2026-02-19 00:00:02'),
                (3, 'Clicked Root Html', 'https://example.com/html-clicked', 'https://example.com/html-clicked', 'html', 0, 4, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01'),
                (4, 'Discovered Html', 'https://example.com/discovered', 'https://example.com/discovered', 'html', 1, 0, NULL, '2026-02-19 00:00:04', '2026-02-19 00:00:04');
        "#,
    )
    .await;

    let mut parsed = ParsedHyperlinkQuery::default();
    parsed.types.push(TypeToken::NonPdf);
    parsed.clicks.push(ClickToken::Unclicked);

    let slice =
        fetch_hyperlink_page_slice(&connection, &parsed, &[], OrderToken::Newest, 1, 10, false)
            .await
            .expect("page slice should load");

    assert_eq!(slice.hyperlink_ids, vec![2]);
    assert_eq!(slice.page, 1);
    assert_eq!(slice.total_pages, 1);
}
