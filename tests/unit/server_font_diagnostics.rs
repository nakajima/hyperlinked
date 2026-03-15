use super::{
    LinuxPackageManager, fc_match_satisfies_family, package_manager_hint, parse_fc_match_family,
    parse_required_families,
};

#[test]
fn parse_required_families_trims_and_deduplicates() {
    let families = parse_required_families(" Noto Sans, Noto Serif, noto sans, ,Noto Color Emoji ");
    assert_eq!(
        families,
        vec!["Noto Sans", "Noto Serif", "Noto Color Emoji"]
    );
}

#[test]
fn parse_fc_match_family_extracts_quoted_family() {
    let parsed = parse_fc_match_family(r#"NotoSans-Regular.ttf: "Noto Sans" "Regular""#);
    assert_eq!(parsed.as_deref(), Some("Noto Sans"));
}

#[test]
fn fc_match_satisfies_family_accepts_exact_and_extended_matches() {
    assert!(fc_match_satisfies_family("Noto Sans", "Noto Sans"));
    assert!(fc_match_satisfies_family("Noto Sans", "Noto Sans CJK JP"));
    assert!(!fc_match_satisfies_family("Noto Sans", "DejaVu Sans"));
}

#[test]
fn package_manager_hint_detects_apt() {
    let os_release = r#"ID=ubuntu
ID_LIKE=debian
"#;
    assert_eq!(
        package_manager_hint(Some(os_release)),
        LinuxPackageManager::Apt
    );
}

#[test]
fn package_manager_hint_detects_dnf() {
    let os_release = r#"ID=fedora
ID_LIKE="rhel fedora"
"#;
    assert_eq!(
        package_manager_hint(Some(os_release)),
        LinuxPackageManager::Dnf
    );
}

#[test]
fn package_manager_hint_detects_apk() {
    let os_release = r#"ID=alpine"#;
    assert_eq!(
        package_manager_hint(Some(os_release)),
        LinuxPackageManager::Apk
    );
}
