use axum::response::Html;
use sailfish::{RenderError, TemplateOnce};
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use crate::app::controllers::flash::{Flash, FlashName};

const POINTER_LOGO_SVG: &str = include_str!("../../server/assets/pointer.svg");
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const ASSET_TOKEN_PATHS: [&str; 3] = [
    "src/server/assets/app.css",
    "src/server/assets/fonts.css",
    "src/server/assets/app.js",
];

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
    asset_version_token: String,
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
    let show_admin_warning_badge =
        crate::server::chromium_diagnostics::current().has_missing_binary();
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
        asset_version_token: asset_version_token(),
    }
    .render_once()
    .map(Html)
}

fn asset_version_token() -> String {
    let source_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut latest_modified_millis = 0u128;
    for relative_path in ASSET_TOKEN_PATHS {
        let Ok(metadata) = std::fs::metadata(source_root.join(relative_path)) else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let Ok(duration) = modified.duration_since(UNIX_EPOCH) else {
            continue;
        };
        latest_modified_millis = latest_modified_millis.max(duration.as_millis());
    }

    if latest_modified_millis == 0 {
        APP_VERSION.to_string()
    } else {
        format!("{APP_VERSION}-{latest_modified_millis}")
    }
}
#[cfg(test)]
#[path = "../../../tests/unit/app_views_layout.rs"]
mod tests;
