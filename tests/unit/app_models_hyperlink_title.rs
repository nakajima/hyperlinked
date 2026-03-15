use super::strip_site_affixes;

#[test]
fn strips_trailing_site_name_when_host_matches() {
    let cleaned = strip_site_affixes(
        "Understanding Rust Lifetimes | Example.com",
        "https://example.com/rust",
        "https://example.com/rust",
    );
    assert_eq!(cleaned, "Understanding Rust Lifetimes");
}

#[test]
fn strips_leading_site_name_when_host_matches() {
    let cleaned = strip_site_affixes(
        "Example.com - Understanding Rust Lifetimes",
        "https://example.com/rust",
        "https://example.com/rust",
    );
    assert_eq!(cleaned, "Understanding Rust Lifetimes");
}

#[test]
fn strips_site_edges_from_multi_segment_titles() {
    let cleaned = strip_site_affixes(
        "Example.com | Understanding Rust Lifetimes | Programming",
        "https://example.com/rust",
        "https://example.com/rust",
    );
    assert_eq!(cleaned, "Understanding Rust Lifetimes - Programming");
}

#[test]
fn keeps_non_site_titles_with_separator() {
    let cleaned = strip_site_affixes(
        "Rust - The Book",
        "https://doc.rust-lang.org/book",
        "https://doc.rust-lang.org/book",
    );
    assert_eq!(cleaned, "Rust - The Book");
}
