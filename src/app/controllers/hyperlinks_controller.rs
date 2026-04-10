use std::collections::{HashMap, HashSet};

#[cfg(test)]
use axum::http::StatusCode;
use axum::{Router, routing};
use sailfish::Template;
use serde::{Deserialize, Serialize};

use crate::{
    app::models::{artifact_job, settings},
    entity::{
        hyperlink::{self, HyperlinkSourceType},
        hyperlink_artifact::{self, HyperlinkArtifactKind},
        hyperlink_processing_job,
    },
    server::{context::Context, hyperlink_fetcher},
};

use crate::server::hyperlink_fetcher::{
    HyperlinkFetchResults, OrderToken, ScopeToken, StatusToken, TypeToken,
};

#[path = "hyperlinks_controller/actions.rs"]
mod actions;
#[path = "hyperlinks_controller/artifacts.rs"]
mod artifacts;
pub(crate) mod params;
pub(crate) mod result;

use crate::app::helpers::hyperlinks::{
    IndexStatus, OgSummary, artifact_delete_path, artifact_download_path, artifact_fetch_path,
    artifact_inline_path, artifact_kind_label, artifact_kind_slug, artifact_pdf_preview_path,
    display_url_host, format_size_bytes, hyperlinks_index_href, index_status,
    normalize_link_title_for_display, processing_state_name, render_relative_time,
    required_show_artifact_kinds, show_artifact_kinds,
};

const SHOW_TIMELINE_LIMIT: u64 = 10;

pub fn routes() -> Router<Context> {
    Router::new()
        .route(
            "/hyperlinks",
            routing::get(actions::index).post(actions::create),
        )
        .route(
            "/hyperlinks.json",
            routing::get(actions::index).post(actions::create),
        )
        .route("/hyperlinks/new", routing::get(actions::new))
        .route("/hyperlinks/lookup", routing::get(actions::lookup))
        .route("/hyperlinks/{id}/click", routing::post(actions::click))
        .route("/hyperlinks/{id}/visit", routing::get(actions::visit))
        .route("/hyperlinks/{id}/edit", routing::get(actions::edit))
        .route("/hyperlinks/{id}/update", routing::post(actions::update))
        .route("/hyperlinks/{id}/delete", routing::post(actions::destroy))
        .route(
            "/hyperlinks/{id}/reprocess",
            routing::post(actions::reprocess),
        )
        .route(
            "/hyperlinks/{id}",
            routing::get(actions::show)
                .patch(actions::update)
                .delete(actions::destroy),
        )
        .route(
            "/hyperlinks/{id}/artifacts/{kind}",
            routing::get(artifacts::download_latest_artifact),
        )
        .route(
            "/hyperlinks/{id}/artifacts/{kind}/inline",
            routing::get(artifacts::render_latest_artifact_inline),
        )
        .route(
            "/hyperlinks/{id}/artifacts/pdf_source/preview",
            routing::get(artifacts::render_pdf_source_preview),
        )
        .route(
            "/hyperlinks/{id}/artifacts/{kind}/delete",
            routing::post(artifacts::delete_artifact_kind),
        )
        .route(
            "/hyperlinks/{id}/artifacts/{kind}/fetch",
            routing::post(artifacts::fetch_artifact_kind),
        )
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct HyperlinkResponse {
    pub(crate) id: i32,
    pub(crate) title: String,
    pub(crate) url: String,
    pub(crate) raw_url: String,
    pub(crate) summary: Option<String>,
    pub(crate) source_type: String,
    pub(crate) clicks_count: i32,
    pub(crate) last_clicked_at: Option<String>,
    pub(crate) processing_state: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct HyperlinksIndexQueryResponse {
    raw_q: String,
    parsed: hyperlink_fetcher::ParsedHyperlinkQuery,
    ignored_tokens: Vec<String>,
    free_text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct HyperlinksIndexResponse {
    items: Vec<HyperlinkResponse>,
    query: HyperlinksIndexQueryResponse,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct HyperlinkLookupResponse {
    status: String,
    id: Option<i32>,
    canonical_url: Option<String>,
}

#[derive(Template)]
#[template(path = "hyperlinks/index.stpl")]
struct HyperlinksIndexTemplate<'a> {
    links: &'a [hyperlink::Model],
    latest_jobs: &'a HashMap<i32, hyperlink_processing_job::Model>,
    active_processing_job_ids: &'a HashSet<i32>,
    thumbnail_artifacts: &'a HashMap<i32, hyperlink_artifact::Model>,
    dark_thumbnail_artifacts: &'a HashMap<i32, hyperlink_artifact::Model>,
    match_snippets: &'a HashMap<i32, String>,
    parsed_query: &'a crate::server::hyperlink_fetcher::ParsedHyperlinkQuery,
    query_input: &'a str,
    ignored_tokens: &'a [String],
    free_text: &'a str,
    page: u64,
    total_pages: u64,
    prev_page_href: Option<String>,
    next_page_href: Option<String>,
}

impl<'a> HyperlinksIndexTemplate<'a> {
    fn index_status(&self, job: Option<&hyperlink_processing_job::Model>) -> Option<IndexStatus> {
        index_status(job, Some(self.active_processing_job_ids))
    }

    fn has_free_text(&self) -> bool {
        !self.free_text.trim().is_empty()
    }

    fn status_select_value(&self) -> &'static str {
        if self.parsed_query.statuses.is_empty()
            || self.parsed_query.statuses.contains(&StatusToken::All)
        {
            return "all";
        }

        if self.parsed_query.statuses.len() != 1 {
            return "all";
        }

        match self.parsed_query.statuses[0] {
            StatusToken::All => "all",
            StatusToken::Processing => "processing",
            StatusToken::Failed => "failed",
            StatusToken::Idle => "idle",
            StatusToken::Succeeded => "succeeded",
        }
    }

    fn type_select_value(&self) -> &'static str {
        let has_all = self.parsed_query.types.contains(&TypeToken::All);
        let has_pdf = self.parsed_query.types.contains(&TypeToken::Pdf);
        let has_non_pdf = self.parsed_query.types.contains(&TypeToken::NonPdf);

        if has_all || (has_pdf && has_non_pdf) || self.parsed_query.types.is_empty() {
            "all"
        } else if has_pdf {
            "pdf"
        } else {
            "non-pdf"
        }
    }

    fn order_select_value(&self) -> &'static str {
        let has_free_text = self.has_free_text();
        let effective_order = match self.parsed_query.orders.last().copied() {
            Some(OrderToken::Relevance) if !has_free_text => OrderToken::Newest,
            Some(order) => order,
            None if has_free_text => OrderToken::Relevance,
            None => OrderToken::Newest,
        };

        match effective_order {
            OrderToken::Newest => "newest",
            OrderToken::Oldest => "oldest",
            OrderToken::MostClicked => "most-clicked",
            OrderToken::RecentlyClicked => "recently-clicked",
            OrderToken::Random => "random",
            OrderToken::Relevance => "relevance",
        }
    }

    fn show_discovered_links_checked(&self) -> bool {
        let has_all = self.parsed_query.scopes.contains(&ScopeToken::All);
        let has_discovered = self.parsed_query.scopes.contains(&ScopeToken::Discovered);

        has_all || has_discovered
    }

    fn mobile_filters_open(&self) -> bool {
        !self.parsed_query.statuses.is_empty()
            || !self.parsed_query.types.is_empty()
            || !self.parsed_query.orders.is_empty()
            || self.show_discovered_links_checked()
    }

    fn has_effective_filter(&self) -> bool {
        let status_filters_active = !self.parsed_query.statuses.is_empty()
            && !self.parsed_query.statuses.contains(&StatusToken::All);

        let has_scope_all = self.parsed_query.scopes.contains(&ScopeToken::All);
        let has_scope_root = self.parsed_query.scopes.contains(&ScopeToken::Root);
        let has_scope_discovered = self.parsed_query.scopes.contains(&ScopeToken::Discovered);
        let scope_filter_active = has_scope_discovered && !has_scope_all && !has_scope_root;

        let has_type_all = self.parsed_query.types.contains(&TypeToken::All);
        let has_type_pdf = self.parsed_query.types.contains(&TypeToken::Pdf);
        let has_type_non_pdf = self.parsed_query.types.contains(&TypeToken::NonPdf);
        let type_filter_active = !has_type_all && (has_type_pdf ^ has_type_non_pdf);

        status_filters_active || scope_filter_active || type_filter_active || self.has_free_text()
    }

    fn link_display_title(&self, link: &hyperlink::Model) -> String {
        normalize_link_title_for_display(
            link.title.as_str(),
            link.url.as_str(),
            link.raw_url.as_str(),
        )
    }

    fn link_display_host(&self, link: &hyperlink::Model) -> String {
        display_url_host(link.url.as_str())
    }

    fn thumbnail_inline_path(&self, hyperlink_id: i32) -> Option<String> {
        self.thumbnail_artifacts
            .contains_key(&hyperlink_id)
            .then_some(artifact_inline_path(
                hyperlink_id,
                &HyperlinkArtifactKind::ScreenshotThumbWebp,
            ))
    }

    fn thumbnail_dark_inline_path(&self, hyperlink_id: i32) -> Option<String> {
        self.dark_thumbnail_artifacts
            .contains_key(&hyperlink_id)
            .then_some(artifact_inline_path(
                hyperlink_id,
                &HyperlinkArtifactKind::ScreenshotThumbDarkWebp,
            ))
    }

    fn link_is_pdf(&self, hyperlink: &hyperlink::Model) -> bool {
        matches!(hyperlink.source_type, HyperlinkSourceType::Pdf)
    }
}

fn render_index(results: &HyperlinkFetchResults) -> Result<String, sailfish::RenderError> {
    let prev_page_href = if results.page > 1 {
        Some(hyperlinks_index_href(&results.raw_q, results.page - 1))
    } else {
        None
    };
    let next_page_href = if results.page < results.total_pages {
        Some(hyperlinks_index_href(&results.raw_q, results.page + 1))
    } else {
        None
    };

    HyperlinksIndexTemplate {
        links: &results.links,
        latest_jobs: &results.latest_jobs,
        active_processing_job_ids: &results.active_processing_job_ids,
        thumbnail_artifacts: &results.thumbnail_artifacts,
        dark_thumbnail_artifacts: &results.dark_thumbnail_artifacts,
        match_snippets: &results.match_snippets,
        parsed_query: &results.parsed_query,
        query_input: &results.raw_q,
        ignored_tokens: &results.ignored_tokens,
        free_text: &results.free_text,
        page: results.page,
        total_pages: results.total_pages,
        prev_page_href,
        next_page_href,
    }
    .render()
}

#[derive(Template)]
#[template(path = "hyperlinks/new.stpl")]
struct HyperlinksNewTemplate<'a> {
    title: &'a str,
    url: &'a str,
    errors: &'a [String],
}

fn render_new() -> Result<String, sailfish::RenderError> {
    render_new_form("", "", &[])
}

fn render_new_form(
    title: &str,
    url: &str,
    errors: &[String],
) -> Result<String, sailfish::RenderError> {
    HyperlinksNewTemplate { title, url, errors }.render()
}

#[derive(Template)]
#[template(path = "hyperlinks/show.stpl")]
struct HyperlinksShowTemplate<'a> {
    link: &'a hyperlink::Model,
    latest_job: Option<&'a hyperlink_processing_job::Model>,
    latest_artifacts: &'a HashMap<HyperlinkArtifactKind, hyperlink_artifact::Model>,
    recent_jobs: &'a [hyperlink_processing_job::Model],
    discovered_links: &'a [hyperlink::Model],
    discovered_latest_jobs: &'a HashMap<i32, hyperlink_processing_job::Model>,
    discovered_thumbnail_artifacts: &'a HashMap<i32, hyperlink_artifact::Model>,
    discovered_dark_thumbnail_artifacts: &'a HashMap<i32, hyperlink_artifact::Model>,
    artifact_settings: settings::ArtifactCollectionSettings,
    display_title: String,
    og_summary: Option<OgSummary>,
}

impl<'a> HyperlinksShowTemplate<'a> {
    fn index_status(&self, job: Option<&hyperlink_processing_job::Model>) -> Option<IndexStatus> {
        index_status(job, None)
    }

    fn display_title(&self) -> &str {
        self.display_title.as_str()
    }

    fn link_display_title(&self, link: &hyperlink::Model) -> String {
        normalize_link_title_for_display(
            link.title.as_str(),
            link.url.as_str(),
            link.raw_url.as_str(),
        )
    }

    fn link_display_host(&self, link: &hyperlink::Model) -> String {
        display_url_host(link.url.as_str())
    }

    fn processing_state(&self) -> &'static str {
        processing_state_name(self.latest_job)
    }

    fn latest_error_message(&self) -> Option<&str> {
        self.latest_job
            .filter(|job| {
                matches!(
                    job.state,
                    hyperlink_processing_job::HyperlinkProcessingJobState::Failed
                )
            })
            .and_then(|job| job.error_message.as_deref())
    }

    fn present_artifact_kinds(&self) -> Vec<HyperlinkArtifactKind> {
        show_artifact_kinds()
            .into_iter()
            .filter(|kind| self.latest_artifacts.contains_key(kind))
            .collect()
    }

    fn missing_artifact_kinds(&self) -> Vec<HyperlinkArtifactKind> {
        required_show_artifact_kinds(&self.link.source_type, self.latest_artifacts)
            .into_iter()
            .filter(|kind| !self.latest_artifacts.contains_key(kind))
            .collect()
    }

    fn artifact_for(&self, kind: &HyperlinkArtifactKind) -> Option<&hyperlink_artifact::Model> {
        self.latest_artifacts.get(kind)
    }

    fn artifact_download_path(&self, kind: &HyperlinkArtifactKind) -> String {
        artifact_download_path(self.link.id, kind)
    }

    fn artifact_inline_path(&self, kind: &HyperlinkArtifactKind) -> String {
        artifact_inline_path(self.link.id, kind)
    }

    fn artifact_delete_path(&self, kind: &HyperlinkArtifactKind) -> String {
        artifact_delete_path(self.link.id, kind)
    }

    fn artifact_fetch_path(&self, kind: &HyperlinkArtifactKind) -> String {
        artifact_fetch_path(self.link.id, kind)
    }

    fn artifact_fetch_enabled(&self, kind: &HyperlinkArtifactKind) -> bool {
        artifact_job::artifact_kind_fetch_enabled(kind, self.artifact_settings)
    }

    fn og_summary(&self) -> Option<&OgSummary> {
        self.og_summary.as_ref()
    }

    fn og_meta_inline_path(&self) -> Option<String> {
        self.latest_artifacts
            .contains_key(&HyperlinkArtifactKind::OgMeta)
            .then_some(self.artifact_inline_path(&HyperlinkArtifactKind::OgMeta))
    }

    fn pdf_preview_path(&self) -> Option<String> {
        self.latest_artifacts
            .contains_key(&HyperlinkArtifactKind::PdfSource)
            .then_some(artifact_pdf_preview_path(self.link.id))
    }

    fn screenshot_inline_path(&self) -> Option<String> {
        self.latest_artifacts
            .contains_key(&HyperlinkArtifactKind::ScreenshotWebp)
            .then_some(self.artifact_inline_path(&HyperlinkArtifactKind::ScreenshotWebp))
    }

    fn screenshot_dark_inline_path(&self) -> Option<String> {
        self.latest_artifacts
            .contains_key(&HyperlinkArtifactKind::ScreenshotDarkWebp)
            .then_some(self.artifact_inline_path(&HyperlinkArtifactKind::ScreenshotDarkWebp))
    }

    fn thumbnail_inline_path(&self, hyperlink_id: i32) -> Option<String> {
        if hyperlink_id == self.link.id
            && self
                .latest_artifacts
                .contains_key(&HyperlinkArtifactKind::ScreenshotThumbWebp)
        {
            return Some(self.artifact_inline_path(&HyperlinkArtifactKind::ScreenshotThumbWebp));
        }

        self.discovered_thumbnail_artifacts
            .contains_key(&hyperlink_id)
            .then_some(artifact_inline_path(
                hyperlink_id,
                &HyperlinkArtifactKind::ScreenshotThumbWebp,
            ))
    }

    fn thumbnail_dark_inline_path(&self, hyperlink_id: i32) -> Option<String> {
        if hyperlink_id == self.link.id
            && self
                .latest_artifacts
                .contains_key(&HyperlinkArtifactKind::ScreenshotThumbDarkWebp)
        {
            return Some(
                self.artifact_inline_path(&HyperlinkArtifactKind::ScreenshotThumbDarkWebp),
            );
        }

        self.discovered_dark_thumbnail_artifacts
            .contains_key(&hyperlink_id)
            .then_some(artifact_inline_path(
                hyperlink_id,
                &HyperlinkArtifactKind::ScreenshotThumbDarkWebp,
            ))
    }

    fn link_is_pdf(&self, hyperlink: &hyperlink::Model) -> bool {
        matches!(hyperlink.source_type, HyperlinkSourceType::Pdf)
    }
}

fn render_show(
    link: &hyperlink::Model,
    latest_job: Option<&hyperlink_processing_job::Model>,
    latest_artifacts: &HashMap<HyperlinkArtifactKind, hyperlink_artifact::Model>,
    recent_jobs: &[hyperlink_processing_job::Model],
    discovered_links: &[hyperlink::Model],
    discovered_latest_jobs: &HashMap<i32, hyperlink_processing_job::Model>,
    discovered_thumbnail_artifacts: &HashMap<i32, hyperlink_artifact::Model>,
    discovered_dark_thumbnail_artifacts: &HashMap<i32, hyperlink_artifact::Model>,
    artifact_settings: settings::ArtifactCollectionSettings,
    display_title: String,
    og_summary: Option<OgSummary>,
) -> Result<String, sailfish::RenderError> {
    HyperlinksShowTemplate {
        link,
        latest_job,
        latest_artifacts,
        recent_jobs,
        discovered_links,
        discovered_latest_jobs,
        discovered_thumbnail_artifacts,
        discovered_dark_thumbnail_artifacts,
        artifact_settings,
        display_title,
        og_summary,
    }
    .render()
}

#[derive(Template)]
#[template(path = "hyperlinks/edit.stpl")]
struct HyperlinksEditTemplate<'a> {
    link_id: i32,
    title: &'a str,
    url: &'a str,
    errors: &'a [String],
}

fn render_edit(link: &hyperlink::Model) -> Result<String, sailfish::RenderError> {
    render_edit_form(link.id, link.title.as_str(), link.raw_url.as_str(), &[])
}

fn render_edit_form(
    link_id: i32,
    title: &str,
    url: &str,
    errors: &[String],
) -> Result<String, sailfish::RenderError> {
    HyperlinksEditTemplate {
        link_id,
        title,
        url,
        errors,
    }
    .render()
}
#[cfg(test)]
#[path = "../../../tests/unit/app_controllers_hyperlinks_controller.rs"]
mod tests;
