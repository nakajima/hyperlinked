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
    active_admin_tab_href: Option<&'a str>,
}

impl BaseLayoutTemplate<'_> {
    fn has_admin_tabs(&self) -> bool {
        self.active_admin_tab_href.is_some()
    }

    fn admin_tab_is_active(&self, href: &str) -> bool {
        self.active_admin_tab_href == Some(href)
    }

    fn admin_tab_link_class(&self, href: &str) -> &'static str {
        if self.admin_tab_is_active(href) {
            "inline-flex h-8 items-center gap-1 rounded-t-[0.3rem] border-b-2 border-accent bg-tertiary/10 px-3 text-accent no-underline"
        } else {
            "inline-flex h-8 items-center gap-1 rounded-t-[0.3rem] border-b-2 border-transparent bg-tertiary/25 px-3 text-accent/80 no-underline hover:bg-form-control hover:text-accent"
        }
    }
}

pub(crate) fn page(
    title: &str,
    body_html: &str,
    flash: &mut Flash,
) -> Result<Html<String>, RenderError> {
    let dev_restart_alert = crate::dev_reload_marker::read_failed_message();
    page_with_dev_restart_alert(title, body_html, flash, dev_restart_alert, None)
}

pub(crate) fn page_with_admin_tabs(
    title: &str,
    body_html: &str,
    flash: &mut Flash,
    active_admin_tab_href: &str,
) -> Result<Html<String>, RenderError> {
    let dev_restart_alert = crate::dev_reload_marker::read_failed_message();
    page_with_dev_restart_alert(
        title,
        body_html,
        flash,
        dev_restart_alert,
        Some(active_admin_tab_href),
    )
}

fn page_with_dev_restart_alert(
    title: &str,
    body_html: &str,
    flash: &mut Flash,
    dev_restart_alert: Option<String>,
    active_admin_tab_href: Option<&str>,
) -> Result<Html<String>, RenderError> {
    let show_admin_warning_badge = super::chromium_diagnostics::current().has_missing_binary();
    page_with_flags(
        title,
        body_html,
        flash,
        dev_restart_alert,
        show_admin_warning_badge,
        active_admin_tab_href,
    )
}

fn page_with_flags(
    title: &str,
    body_html: &str,
    flash: &mut Flash,
    dev_restart_alert: Option<String>,
    show_admin_warning_badge: bool,
    active_admin_tab_href: Option<&str>,
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
        active_admin_tab_href,
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
    fn admin_nav_points_to_admin_artifacts_without_queue_badge_placeholder() {
        let mut flash = Flash::default();
        let html = page_with_flags("Title", "<p>Body</p>", &mut flash, None, false, None)
            .expect("layout should render")
            .0;
        assert!(html.contains("href=\"/admin/artifacts\""));
        assert!(!html.contains("href=\"/admin/jobs\""));
        assert!(!html.contains("data-queue-pending-badge"));
    }

    #[test]
    fn includes_favicon_link_tags() {
        let mut flash = Flash::default();
        let html = page_with_flags("Title", "<p>Body</p>", &mut flash, None, false, None)
            .expect("layout should render")
            .0;
        assert!(html.contains("href=\"/assets/favicon.png\""));
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
            Some("/admin/tags"),
        )
        .expect("layout should render")
        .0;
        assert!(html.contains("aria-label=\"Admin sections\""));
        assert!(html.contains("href=\"/admin/overview\""));
        assert!(html.contains("href=\"/admin/artifacts\""));
        assert!(html.contains("href=\"/admin/tags\""));
        assert!(html.contains("href=\"/admin/queue\""));
        assert!(html.contains("href=\"/admin/import-export\""));
        assert!(html.contains("href=\"/admin/storage\""));
        assert!(html.contains("data-queue-pending-badge"));
    }
}
