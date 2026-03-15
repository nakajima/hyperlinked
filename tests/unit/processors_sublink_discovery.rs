use super::*;

#[test]
fn extracts_inline_and_autolink_urls() {
    let markdown = "[Example](https://example.com/a)\n[Relative](/docs/start)\n![Screenshot](https://cdn.example.com/screenshot.png)\n<https://example.net/x>\n";
    let urls = extract_candidate_urls(markdown);
    assert!(urls.contains(&"https://example.com/a".to_string()));
    assert!(urls.contains(&"/docs/start".to_string()));
    assert!(urls.contains(&"https://cdn.example.com/screenshot.png".to_string()));
    assert!(urls.contains(&"https://example.net/x".to_string()));
}

#[test]
fn high_precision_mode_ignores_plain_text_urls() {
    let text = "Read https://example.com/a, then (https://example.org/b).";
    let urls = extract_candidate_urls(text);
    assert!(!urls.contains(&"https://example.com/a".to_string()));
    assert!(!urls.contains(&"https://example.org/b".to_string()));
}

#[test]
fn extracts_doi_citations_as_urls() {
    let text = "DOI: 10.1000/xyz123\nsee doi:10.2000/abc456.";
    let urls = extract_candidate_urls(text);
    assert!(urls.contains(&"https://doi.org/10.1000/xyz123".to_string()));
    assert!(urls.contains(&"https://doi.org/10.2000/abc456".to_string()));
}

#[test]
fn extracts_arxiv_citations_as_urls() {
    let text = "arXiv:1706.03762v7 and arxiv:cs/9301115.";
    let urls = extract_candidate_urls(text);
    assert!(urls.contains(&"https://arxiv.org/abs/1706.03762v7".to_string()));
    assert!(urls.contains(&"https://arxiv.org/abs/cs/9301115".to_string()));
}

#[test]
fn malformed_plain_text_markdown_tails_are_not_extracted() {
    let text = "https://github.com/rtk-ai/rtk)**\nhttps://github.com/getnao/nao)[nao](https://github.com/getnao/nao)\n";

    let urls = extract_candidate_urls(text);
    assert_eq!(urls, vec!["https://github.com/getnao/nao".to_string()]);
}

#[test]
fn normalizes_relative_urls_against_base() {
    let normalized = normalize_candidate_url("https://example.com/posts/1", "../about")
        .expect("relative url should resolve");
    assert_eq!(normalized, "https://example.com/about");
}

#[test]
fn accepts_absolute_candidates_when_base_is_relative() {
    let normalized = normalize_candidate_url("/uploads/12/doc.pdf", "https://example.com/paper")
        .expect("absolute candidate should normalize");
    assert_eq!(normalized, "https://example.com/paper");
}

#[test]
fn rejects_relative_candidates_when_base_is_relative() {
    assert!(normalize_candidate_url("/uploads/12/doc.pdf", "../ref").is_none());
}

#[test]
fn rejects_non_http_schemes() {
    assert!(normalize_candidate_url("https://example.com", "mailto:hi@example.com").is_none());
    assert!(normalize_candidate_url("https://example.com", "javascript:alert(1)").is_none());
}

#[test]
fn strips_fragments() {
    let normalized = normalize_candidate_url("https://example.com", "/guide#intro")
        .expect("url should normalize");
    assert_eq!(normalized, "https://example.com/guide");
}
