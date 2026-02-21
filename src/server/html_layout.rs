use axum::response::Html;
use sailfish::{RenderError, TemplateOnce};

const POINTER_LOGO_SVG: &str = include_str!("assets/pointer.svg");

#[derive(TemplateOnce)]
#[template(path = "layout/base.stpl")]
struct BaseLayoutTemplate<'a> {
    title: &'a str,
    body_html: &'a str,
    pointer_logo_svg: &'a str,
}

pub(crate) fn page(title: &str, body_html: &str) -> Result<Html<String>, RenderError> {
    BaseLayoutTemplate {
        title,
        body_html,
        pointer_logo_svg: POINTER_LOGO_SVG,
    }
    .render_once()
    .map(Html)
}
