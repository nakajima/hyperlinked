use axum::{Form, Router, extract::State, http::HeaderMap, routing};
use sailfish::Template;
use serde::Deserialize;

use crate::{
    app::helpers::admin_dashboard::ADMIN_TAB_FEEDS_PATH,
    entity::rss_feed,
    server::{
        context::Context,
        flash::{Flash, FlashName, redirect_with_flash},
        views,
    },
};

pub fn routes() -> Router<Context> {
    Router::new()
        .route(ADMIN_TAB_FEEDS_PATH, routing::get(index))
        .route("/admin/feeds/create", routing::post(create))
        .route("/admin/feeds/{id}/sync", routing::post(sync))
        .route("/admin/feeds/{id}/toggle", routing::post(toggle_active))
        .route("/admin/feeds/{id}/delete", routing::post(delete))
}

#[derive(Debug, Deserialize)]
struct CreateFeedForm {
    url: String,
    backfill: Option<String>,
}

struct FeedRow {
    feed: rss_feed::Model,
    hyperlink_count: u64,
}

#[derive(Template)]
#[template(path = "admin/_tab_feeds.stpl")]
struct FeedsTabTemplate {
    feeds: Vec<FeedRow>,
}

impl FeedsTabTemplate {
    fn format_datetime(&self, dt: &sea_orm::entity::prelude::DateTime) -> String {
        dt.format("%Y-%m-%d %H:%M").to_string()
    }

    fn format_optional_datetime(&self, dt: &Option<sea_orm::entity::prelude::DateTime>) -> String {
        match dt {
            Some(dt) => self.format_datetime(dt),
            None => "Never".to_string(),
        }
    }
}

async fn index(State(state): State<Context>, headers: HeaderMap) -> axum::response::Response {
    let feeds = match rss_feed::list(&state.connection).await {
        Ok(feeds) => feeds,
        Err(err) => {
            return views::render_html_page_with_admin_tabs_and_flash(
                "Admin",
                ADMIN_TAB_FEEDS_PATH,
                Err(sailfish::RenderError::Msg(format!(
                    "failed to load feeds: {err}"
                ))),
                Flash::from_headers(&headers),
            );
        }
    };

    let mut feed_rows = Vec::with_capacity(feeds.len());
    for feed in feeds {
        let hyperlink_count = rss_feed::hyperlink_count_for_feed(&state.connection, feed.id)
            .await
            .unwrap_or(0);
        feed_rows.push(FeedRow {
            feed,
            hyperlink_count,
        });
    }

    let body = FeedsTabTemplate { feeds: feed_rows }.render();

    views::render_html_page_with_admin_tabs_and_flash(
        "Admin",
        ADMIN_TAB_FEEDS_PATH,
        body,
        Flash::from_headers(&headers),
    )
}

async fn create(
    State(state): State<Context>,
    headers: HeaderMap,
    Form(form): Form<CreateFeedForm>,
) -> axum::response::Response {
    let url = form.url.trim();
    if url.is_empty() {
        return redirect_with_flash(
            &headers,
            ADMIN_TAB_FEEDS_PATH,
            FlashName::Alert,
            "Feed URL is required.",
        );
    }

    let backfill = form.backfill.as_deref() == Some("on");

    match rss_feed::create(
        &state.connection,
        url,
        backfill,
        state.processing_queue.as_ref(),
    )
    .await
    {
        Ok((feed, report)) => redirect_with_flash(
            &headers,
            ADMIN_TAB_FEEDS_PATH,
            FlashName::Notice,
            format!(
                "Added feed \"{}\". Imported {} new link(s), {} already existed, {} skipped, {} failed.",
                feed.title,
                report.inserted,
                report.skipped_existing,
                report.skipped_before_cutoff,
                report.failed
            ),
        ),
        Err(err) => redirect_with_flash(
            &headers,
            ADMIN_TAB_FEEDS_PATH,
            FlashName::Alert,
            format!("Failed to add feed: {err}"),
        ),
    }
}

async fn sync(
    State(state): State<Context>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<i32>,
) -> axum::response::Response {
    match rss_feed::sync_by_id(&state.connection, id, state.processing_queue.as_ref()).await {
        Ok(Some(report)) => redirect_with_flash(
            &headers,
            ADMIN_TAB_FEEDS_PATH,
            FlashName::Notice,
            format!(
                "Synced feed. {} new link(s), {} already existed, {} skipped, {} failed.",
                report.inserted,
                report.skipped_existing,
                report.skipped_before_cutoff,
                report.failed
            ),
        ),
        Ok(None) => redirect_with_flash(
            &headers,
            ADMIN_TAB_FEEDS_PATH,
            FlashName::Alert,
            "Feed not found.",
        ),
        Err(err) => redirect_with_flash(
            &headers,
            ADMIN_TAB_FEEDS_PATH,
            FlashName::Alert,
            format!("Sync failed: {err}"),
        ),
    }
}

async fn toggle_active(
    State(state): State<Context>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<i32>,
) -> axum::response::Response {
    match rss_feed::toggle_active(&state.connection, id).await {
        Ok(Some(feed)) => {
            let label = if feed.active { "resumed" } else { "paused" };
            redirect_with_flash(
                &headers,
                ADMIN_TAB_FEEDS_PATH,
                FlashName::Notice,
                format!("Feed \"{}\" {}.", feed.title, label),
            )
        }
        Ok(None) => redirect_with_flash(
            &headers,
            ADMIN_TAB_FEEDS_PATH,
            FlashName::Alert,
            "Feed not found.",
        ),
        Err(err) => redirect_with_flash(
            &headers,
            ADMIN_TAB_FEEDS_PATH,
            FlashName::Alert,
            format!("Failed to toggle feed: {err}"),
        ),
    }
}

async fn delete(
    State(state): State<Context>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<i32>,
) -> axum::response::Response {
    match rss_feed::delete_by_id(&state.connection, id).await {
        Ok(true) => redirect_with_flash(
            &headers,
            ADMIN_TAB_FEEDS_PATH,
            FlashName::Notice,
            "Feed deleted. Imported links have been kept.",
        ),
        Ok(false) => redirect_with_flash(
            &headers,
            ADMIN_TAB_FEEDS_PATH,
            FlashName::Alert,
            "Feed not found.",
        ),
        Err(err) => redirect_with_flash(
            &headers,
            ADMIN_TAB_FEEDS_PATH,
            FlashName::Alert,
            format!("Failed to delete feed: {err}"),
        ),
    }
}
