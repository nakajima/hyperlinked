use super::*;
use crate::app::controllers::flash::Flash;

#[test]
fn omits_dev_restart_banner_when_alert_is_absent() {
    let mut flash = Flash::default();
    let html = page_with_dev_restart_alert("Title", "<p>Body</p>", &mut flash, None, None)
        .expect("layout should render")
        .0;
    assert!(!html.contains("text-dev-alert"));
}

#[test]
fn renders_dev_restart_banner_when_alert_is_present() {
    let mut flash = Flash::default();
    let html = page_with_dev_restart_alert(
        "Title",
        "<p>Body</p>",
        &mut flash,
        Some("Hot reload restart failed: test failure".to_string()),
        None,
    )
    .expect("layout should render")
    .0;
    assert!(html.contains("text-dev-alert"));
    assert!(html.contains("Hot reload restart failed: test failure"));
}

#[test]
fn renders_admin_warning_badge_when_requested() {
    let mut flash = Flash::default();
    let html = page_with_flags("Title", "<p>Body</p>", &mut flash, None, true, None)
        .expect("layout should render")
        .0;
    assert!(html.contains("data-admin-warning-badge"));
}

#[test]
fn omits_admin_warning_badge_when_not_requested() {
    let mut flash = Flash::default();
    let html = page_with_flags("Title", "<p>Body</p>", &mut flash, None, false, None)
        .expect("layout should render")
        .0;
    assert!(!html.contains("data-admin-warning-badge"));
}

#[test]
fn admin_nav_points_to_admin_root_without_queue_badge_placeholder() {
    let mut flash = Flash::default();
    let html = page_with_flags("Title", "<p>Body</p>", &mut flash, None, false, None)
        .expect("layout should render")
        .0;
    assert!(html.contains("href=\"/admin\""));
    assert!(!html.contains("href=\"/admin/jobs\""));
    assert!(!html.contains("data-queue-pending-badge"));
}

#[test]
fn includes_favicon_link_tags() {
    let mut flash = Flash::default();
    let html = page_with_flags("Title", "<p>Body</p>", &mut flash, None, false, None)
        .expect("layout should render")
        .0;
    assert!(html.contains("href=\"/assets/favicon.png?v="));
    assert!(html.contains("href=\"/assets/fonts.css?v="));
    assert!(html.contains("href=\"/assets/app.css?v="));
    assert!(html.contains("src=\"/assets/app.js?v="));
    assert!(html.contains("href=\"/favicon.ico\""));
}

#[test]
fn renders_admin_tabs_inside_header_when_active_tab_is_set() {
    let mut flash = Flash::default();
    let html = page_with_flags(
        "Admin",
        "<p>Body</p>",
        &mut flash,
        None,
        false,
        Some("/admin/llm-interactions"),
    )
    .expect("layout should render")
    .0;
    assert!(html.contains("aria-label=\"Admin sections\""));
    assert!(html.contains("href=\"/admin/overview\""));
    assert!(html.contains("href=\"/admin/artifacts\""));
    assert!(html.contains("href=\"/admin/llm-interactions\""));
    assert!(html.contains("href=\"/admin/queue\""));
    assert!(html.contains("href=\"/admin/import-export\""));
    assert!(html.contains("href=\"/admin/storage\""));
    assert!(html.contains("data-queue-pending-badge"));
}
