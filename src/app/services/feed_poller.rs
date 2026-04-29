use sea_orm::DatabaseConnection;
use std::time::Duration;
use tokio::task::JoinHandle;

use crate::{app::models::hyperlink_processing_job::ProcessingQueueSender, entity::rss_feed};

const DEFAULT_POLL_CHECK_INTERVAL_SECS: u64 = 60;

pub fn spawn(
    connection: DatabaseConnection,
    processing_queue: Option<ProcessingQueueSender>,
) -> JoinHandle<()> {
    let interval = Duration::from_secs(
        std::env::var("FEED_POLL_CHECK_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_POLL_CHECK_INTERVAL_SECS)
            .max(10),
    );

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            poll_due_feeds(&connection, processing_queue.as_ref()).await;
        }
    })
}

async fn poll_due_feeds(
    connection: &DatabaseConnection,
    processing_queue: Option<&ProcessingQueueSender>,
) {
    let due_feeds = match rss_feed::list_due_for_poll(connection).await {
        Ok(feeds) => feeds,
        Err(err) => {
            tracing::warn!(error = %err, "failed to list feeds due for poll");
            return;
        }
    };

    if due_feeds.is_empty() {
        return;
    }

    tracing::info!(count = due_feeds.len(), "polling due RSS feeds");

    for feed in &due_feeds {
        match rss_feed::sync_by_id(connection, feed.id, processing_queue).await {
            Ok(Some(report)) => {
                tracing::info!(
                    feed_id = feed.id,
                    feed_title = %feed.title,
                    inserted = report.inserted,
                    skipped_existing = report.skipped_existing,
                    failed = report.failed,
                    "feed poll complete"
                );
            }
            Ok(None) => {
                tracing::warn!(feed_id = feed.id, "feed disappeared during poll");
            }
            Err(err) => {
                tracing::warn!(
                    feed_id = feed.id,
                    feed_title = %feed.title,
                    error = %err,
                    "feed poll failed"
                );
            }
        }
    }
}
