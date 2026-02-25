use axum::response::Html;
use sailfish::{RenderError, TemplateOnce};

use crate::server::flash::{Flash, FlashName};

const POINTER_LOGO_SVG: &str = include_str!("assets/pointer.svg");

#[derive(TemplateOnce)]
#[template(path = "layout/base.stpl")]
struct BaseLayoutTemplate<'a> {
    title: &'a str,
    body_html: &'a str,
    pointer_logo_svg: &'a str,
    dev_restart_alert: Option<String>,
    notice_flash: Option<String>,
    alert_flash: Option<String>,
    show_admin_warning_badge: bool,
}

pub(crate) fn page(
    title: &str,
    body_html: &str,
    flash: &mut Flash,
) -> Result<Html<String>, RenderError> {
    let dev_restart_alert = crate::dev_reload_marker::read_failed_message();
    page_with_dev_restart_alert(title, body_html, flash, dev_restart_alert)
}

fn page_with_dev_restart_alert(
    title: &str,
    body_html: &str,
    flash: &mut Flash,
    dev_restart_alert: Option<String>,
) -> Result<Html<String>, RenderError> {
    let show_admin_warning_badge = super::chromium_diagnostics::current().has_missing_binary();
    page_with_flags(
        title,
        body_html,
        flash,
        dev_restart_alert,
        show_admin_warning_badge,
    )
}

fn page_with_flags(
    title: &str,
    body_html: &str,
    flash: &mut Flash,
    dev_restart_alert: Option<String>,
    show_admin_warning_badge: bool,
) -> Result<Html<String>, RenderError> {
    let notice_flash = flash.render_flash(FlashName::Notice);
    let alert_flash = flash.render_flash(FlashName::Alert);
    BaseLayoutTemplate {
        title,
        body_html,
        pointer_logo_svg: POINTER_LOGO_SVG,
        dev_restart_alert,
        notice_flash,
        alert_flash,
        show_admin_warning_badge,
    }
    .render_once()
    .map(Html)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::flash::Flash;

    #[test]
    fn omits_dev_restart_banner_when_alert_is_absent() {
        let mut flash = Flash::default();
        let html = page_with_dev_restart_alert("Title", "<p>Body</p>", &mut flash, None)
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
        )
        .expect("layout should render")
        .0;
        assert!(html.contains("text-dev-alert"));
        assert!(html.contains("Hot reload restart failed: test failure"));
    }

    #[test]
    fn renders_admin_warning_badge_when_requested() {
        let mut flash = Flash::default();
        let html = page_with_flags("Title", "<p>Body</p>", &mut flash, None, true)
            .expect("layout should render")
            .0;
        assert!(html.contains("data-admin-warning-badge"));
    }

    #[test]
    fn omits_admin_warning_badge_when_not_requested() {
        let mut flash = Flash::default();
        let html = page_with_flags("Title", "<p>Body</p>", &mut flash, None, false)
            .expect("layout should render")
            .0;
        assert!(!html.contains("data-admin-warning-badge"));
    }

    #[test]
    fn queue_nav_points_to_admin_jobs_with_pending_badge_placeholder() {
        let mut flash = Flash::default();
        let html = page_with_flags("Title", "<p>Body</p>", &mut flash, None, false)
            .expect("layout should render")
            .0;
        assert!(html.contains("href=\"/admin/jobs\""));
        assert!(html.contains("data-queue-pending-badge"));
    }
}
