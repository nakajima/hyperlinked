use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, entity::prelude::DateTime,
};

use crate::{
    app::models::{
        hyperlink::{self, HyperlinkInput, NormalizedHyperlinkInput},
        hyperlink_processing_job::ProcessingQueueSender,
    },
    entity::{hyperlink as hyperlink_entity, rss_feed},
};

#[derive(Clone, Debug)]
pub struct ParsedFeedEntry {
    pub url: String,
    pub title: String,
    pub published_at: Option<DateTime>,
}

#[derive(Clone, Debug)]
pub struct ParsedFeed {
    pub title: String,
    pub site_url: Option<String>,
    pub entries: Vec<ParsedFeedEntry>,
}

#[derive(Clone, Debug, Default)]
pub struct FeedSyncReport {
    pub total: usize,
    pub inserted: usize,
    pub skipped_existing: usize,
    pub skipped_before_cutoff: usize,
    pub failed: usize,
}

pub async fn fetch_and_parse(url: &str) -> Result<ParsedFeed, String> {
    let response = reqwest::get(url)
        .await
        .map_err(|err| format!("failed to fetch feed: {err}"))?;
    let body = response
        .bytes()
        .await
        .map_err(|err| format!("failed to read feed body: {err}"))?;
    parse_feed_bytes(&body)
}

pub fn parse_feed_bytes(bytes: &[u8]) -> Result<ParsedFeed, String> {
    let feed =
        feed_rs::parser::parse(bytes).map_err(|err| format!("failed to parse feed: {err}"))?;

    let title = feed.title.map(|t| t.content).unwrap_or_default();

    let site_url = feed.links.first().map(|link| link.href.clone());

    let entries = feed
        .entries
        .into_iter()
        .filter_map(|entry| {
            let url = entry
                .links
                .first()
                .map(|link| link.href.clone())
                .or_else(|| entry.id.starts_with("http").then(|| entry.id.clone()))?;

            let title = entry.title.map(|t| t.content).unwrap_or_default();

            let published_at = entry.published.or(entry.updated).map(|dt| dt.naive_utc());

            Some(ParsedFeedEntry {
                url,
                title,
                published_at,
            })
        })
        .collect();

    Ok(ParsedFeed {
        title,
        site_url,
        entries,
    })
}

pub async fn sync_feed(
    connection: &DatabaseConnection,
    feed: &rss_feed::Model,
    backfill: bool,
    processing_queue: Option<&ProcessingQueueSender>,
) -> Result<FeedSyncReport, String> {
    let parsed = fetch_and_parse(&feed.url).await?;

    let cutoff = if backfill {
        None
    } else {
        Some(feed.created_at)
    };

    let mut report = FeedSyncReport {
        total: parsed.entries.len(),
        ..Default::default()
    };

    for entry in &parsed.entries {
        if let Some(cutoff) = cutoff {
            if let Some(published) = entry.published_at {
                if published < cutoff {
                    report.skipped_before_cutoff += 1;
                    continue;
                }
            }
        }

        let input = HyperlinkInput {
            title: entry.title.clone(),
            url: entry.url.clone(),
        };

        let normalized = match hyperlink::validate_and_normalize(input).await {
            Ok(n) => n,
            Err(_err) => {
                report.failed += 1;
                continue;
            }
        };

        // Check if hyperlink already exists
        let existing = hyperlink_entity::Entity::find()
            .filter(hyperlink_entity::Column::Url.eq(normalized.url.clone()))
            .order_by_asc(hyperlink_entity::Column::Id)
            .one(connection)
            .await
            .map_err(|err| format!("database error: {err}"))?;

        if let Some(existing) = existing {
            // If it exists but has no feed association, claim it for this feed
            if existing.rss_feed_id.is_none() {
                let mut active: hyperlink_entity::ActiveModel = existing.into();
                active.rss_feed_id = Set(Some(feed.id));
                active
                    .update(connection)
                    .await
                    .map_err(|err| format!("database error: {err}"))?;
            }
            report.skipped_existing += 1;
            continue;
        }

        match insert_feed_hyperlink(
            connection,
            normalized,
            feed.id,
            entry.published_at,
            processing_queue,
        )
        .await
        {
            Ok(_) => report.inserted += 1,
            Err(_err) => report.failed += 1,
        }
    }

    // Update last_fetched_at
    let now = chrono::Utc::now().naive_utc();
    let mut active_feed: rss_feed::ActiveModel = feed.clone().into();
    active_feed.last_fetched_at = Set(Some(now));
    active_feed.updated_at = Set(now);
    active_feed
        .update(connection)
        .await
        .map_err(|err| format!("failed to update feed: {err}"))?;

    Ok(report)
}

async fn insert_feed_hyperlink(
    connection: &DatabaseConnection,
    input: NormalizedHyperlinkInput,
    rss_feed_id: i32,
    created_at: Option<DateTime>,
    processing_queue: Option<&ProcessingQueueSender>,
) -> Result<hyperlink_entity::Model, sea_orm::DbErr> {
    let now = chrono::Utc::now().naive_utc();
    let created_at = created_at.unwrap_or(now);

    let model = hyperlink_entity::ActiveModel {
        title: Set(input.title),
        url: Set(input.url),
        raw_url: Set(input.raw_url),
        rss_feed_id: Set(Some(rss_feed_id)),
        discovery_depth: Set(hyperlink::ROOT_DISCOVERY_DEPTH),
        clicks_count: Set(0),
        created_at: Set(created_at),
        updated_at: Set(now),
        ..Default::default()
    };

    let inserted = model.insert(connection).await?;

    // Enqueue processing if enabled
    if let Some(queue) = processing_queue {
        let settings = crate::app::models::settings::load(connection).await?;
        if settings.collect_source {
            let _ =
                crate::app::models::artifact_job::resolve_and_enqueue_for_job_kind_with_settings(
                    connection,
                    inserted.id,
                    crate::entity::hyperlink_processing_job::HyperlinkProcessingJobKind::Snapshot,
                    crate::app::models::artifact_job::ArtifactFetchMode::RefetchTarget,
                    settings,
                    Some(queue),
                )
                .await?;
        }
    }

    Ok(inserted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rss2_feed() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Test Feed</title>
    <link>https://example.com</link>
    <item>
      <title>First Post</title>
      <link>https://example.com/first</link>
      <pubDate>Mon, 01 Jan 2024 00:00:00 GMT</pubDate>
    </item>
    <item>
      <title>Second Post</title>
      <link>https://example.com/second</link>
    </item>
  </channel>
</rss>"#;

        let feed = parse_feed_bytes(xml).unwrap();
        assert_eq!(feed.title, "Test Feed");
        assert_eq!(feed.site_url.as_deref(), Some("https://example.com/"));
        assert_eq!(feed.entries.len(), 2);
        assert_eq!(feed.entries[0].title, "First Post");
        assert_eq!(feed.entries[0].url, "https://example.com/first");
        assert!(feed.entries[0].published_at.is_some());
        assert_eq!(feed.entries[1].title, "Second Post");
    }

    #[test]
    fn parse_atom_feed() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Atom Feed</title>
  <link href="https://example.com"/>
  <entry>
    <title>Atom Entry</title>
    <link href="https://example.com/atom-entry"/>
    <id>https://example.com/atom-entry</id>
    <updated>2024-01-01T00:00:00Z</updated>
  </entry>
</feed>"#;

        let feed = parse_feed_bytes(xml).unwrap();
        assert_eq!(feed.title, "Atom Feed");
        assert_eq!(feed.entries.len(), 1);
        assert_eq!(feed.entries[0].title, "Atom Entry");
        assert_eq!(feed.entries[0].url, "https://example.com/atom-entry");
        assert!(feed.entries[0].published_at.is_some());
    }
}
