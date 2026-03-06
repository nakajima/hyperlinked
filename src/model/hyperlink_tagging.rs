use std::collections::{HashMap, HashSet};

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr,
    EntityTrait, QueryFilter, QueryOrder, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};

use crate::{
    entity::{
        hyperlink_artifact::HyperlinkArtifactKind,
        hyperlink_tag::{self, HyperlinkTagSource},
        tag,
    },
    model::hyperlink_artifact,
};

pub const TAG_META_CONTENT_TYPE: &str = "application/json";
pub const TAGGING_PROMPT_VERSION: &str = "v1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RankedTag {
    pub tag: String,
    pub confidence: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TagState {
    User,
    AiApproved,
    AiPending,
    AiRejected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaggingSource {
    User,
    Ai,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkTag {
    pub tag: String,
    pub source: TaggingSource,
    pub state: TagState,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TagSet {
    pub all_tags: Vec<LinkTag>,
    pub visible_tags: Vec<String>,
    pub user_tags: Vec<String>,
    pub primary_visible_tag: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingTag {
    pub id: i32,
    pub name: String,
    pub hyperlink_count: usize,
}

#[derive(Clone, Debug)]
pub struct PersistedRankedTag {
    pub tag: String,
    pub confidence: f32,
    pub state_if_new: TagState,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum LegacyTaggingSource {
    Llm,
    Manual,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct LegacyTagMeta {
    source: LegacyTaggingSource,
    ranked_tags: Vec<RankedTag>,
    primary_tag: Option<String>,
    overall_confidence: Option<f32>,
    rationale: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    prompt_version: Option<String>,
    manual_override: bool,
    classified_at: String,
}

#[derive(Clone, Debug)]
pub struct LlmPersistInput {
    pub ranked_tags: Vec<PersistedRankedTag>,
    pub overall_confidence: Option<f32>,
    pub rationale: Option<String>,
    pub provider: String,
    pub model: String,
    pub prompt_version: String,
    pub classified_at: String,
}

pub async fn latest_for_hyperlink(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
) -> Result<Option<TagSet>, DbErr> {
    let map = latest_for_hyperlinks(connection, &[hyperlink_id]).await?;
    Ok(map.get(&hyperlink_id).cloned())
}

pub async fn latest_for_hyperlinks(
    connection: &DatabaseConnection,
    hyperlink_ids: &[i32],
) -> Result<HashMap<i32, TagSet>, DbErr> {
    if hyperlink_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = hyperlink_tag::Entity::find()
        .filter(hyperlink_tag::Column::HyperlinkId.is_in(hyperlink_ids.to_vec()))
        .find_also_related(tag::Entity)
        .all(connection)
        .await?;

    let mut by_hyperlink: HashMap<i32, Vec<LinkTag>> = HashMap::new();
    for (link_tag, maybe_tag) in rows {
        let Some(model) = maybe_tag else {
            continue;
        };
        by_hyperlink
            .entry(link_tag.hyperlink_id)
            .or_default()
            .push(LinkTag {
                tag: model.name,
                source: map_source_from_model(link_tag.source),
                state: map_state_from_model(model.state),
            });
    }

    let mut sets = HashMap::with_capacity(by_hyperlink.len());
    for (hyperlink_id, mut tags) in by_hyperlink {
        normalize_link_tags(&mut tags);
        sets.insert(hyperlink_id, build_tag_set(tags));
    }

    let missing = hyperlink_ids
        .iter()
        .copied()
        .filter(|id| !sets.contains_key(id))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        let legacy = legacy_for_hyperlinks(connection, &missing).await?;
        for (hyperlink_id, set) in legacy {
            sets.insert(hyperlink_id, set);
        }
    }

    Ok(sets)
}

pub async fn list_pending_tags(connection: &DatabaseConnection) -> Result<Vec<PendingTag>, DbErr> {
    let backend = connection.get_database_backend();
    let rows = connection
        .query_all(Statement::from_string(
            backend,
            r#"
                SELECT
                    t.id AS id,
                    t.name AS name,
                    COUNT(ht.id) AS hyperlink_count
                FROM tag t
                LEFT JOIN hyperlink_tag ht
                    ON ht.tag_id = t.id
                WHERE t.state = 'AI_PENDING'
                GROUP BY t.id, t.name
                ORDER BY t.name ASC
            "#
            .to_string(),
        ))
        .await?;

    let mut pending = Vec::with_capacity(rows.len());
    for row in rows {
        let id: i32 = row.try_get("", "id")?;
        let name: String = row.try_get("", "name")?;
        let hyperlink_count: i64 = row.try_get("", "hyperlink_count")?;
        pending.push(PendingTag {
            id,
            name,
            hyperlink_count: hyperlink_count.max(0) as usize,
        });
    }

    Ok(pending)
}

pub async fn approve_pending_tags(
    connection: &DatabaseConnection,
    tag_ids: &[i32],
) -> Result<Vec<String>, DbErr> {
    if tag_ids.is_empty() {
        return Ok(Vec::new());
    }

    let ids = dedupe_i32(tag_ids);
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let now = now_utc();
    let pending_rows = tag::Entity::find()
        .filter(tag::Column::Id.is_in(ids.clone()))
        .filter(tag::Column::State.eq(tag::TagState::AiPending))
        .order_by_asc(tag::Column::Name)
        .all(connection)
        .await?;

    if pending_rows.is_empty() {
        return Ok(Vec::new());
    }

    let approved_names = pending_rows
        .iter()
        .map(|row| row.name.clone())
        .collect::<Vec<_>>();

    tag::Entity::update_many()
        .col_expr(
            tag::Column::State,
            sea_orm::sea_query::Expr::value(tag::TagState::AiApproved),
        )
        .col_expr(tag::Column::UpdatedAt, sea_orm::sea_query::Expr::value(now))
        .filter(tag::Column::Id.is_in(ids))
        .filter(tag::Column::State.eq(tag::TagState::AiPending))
        .exec(connection)
        .await?;

    Ok(approved_names)
}

pub async fn persist_manual_tags(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    tags: Vec<String>,
    classified_at: String,
) -> Result<(), DbErr> {
    let tags = dedupe_tags(tags);
    let ranked_tags = tags
        .iter()
        .enumerate()
        .map(|(index, tag)| RankedTag {
            tag: tag.clone(),
            confidence: (1.0 - (index as f32 * 0.1)).max(0.0),
        })
        .collect::<Vec<_>>();

    let txn = connection.begin().await?;
    hyperlink_tag::Entity::delete_many()
        .filter(hyperlink_tag::Column::HyperlinkId.eq(hyperlink_id))
        .exec(&txn)
        .await?;

    for tag_name in &tags {
        let tag_id = ensure_tag_with_state(&txn, tag_name, TagState::User).await?;
        insert_hyperlink_tag(
            &txn,
            hyperlink_id,
            tag_id,
            HyperlinkTagSource::User,
            now_utc(),
        )
        .await?;
    }

    txn.commit().await?;

    let primary_tag = ranked_tags.first().map(|value| value.tag.clone());
    let meta = LegacyTagMeta {
        source: LegacyTaggingSource::Manual,
        ranked_tags,
        primary_tag,
        overall_confidence: None,
        rationale: None,
        provider: None,
        model: None,
        prompt_version: Some(TAGGING_PROMPT_VERSION.to_string()),
        manual_override: true,
        classified_at,
    };

    persist_tag_meta(connection, hyperlink_id, None, &meta).await
}

pub async fn persist_llm_tags(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    job_id: Option<i32>,
    input: LlmPersistInput,
) -> Result<(), DbErr> {
    let ranked_tags = dedupe_ranked_persisted_tags(input.ranked_tags);
    let now = now_utc();

    let txn = connection.begin().await?;
    hyperlink_tag::Entity::delete_many()
        .filter(hyperlink_tag::Column::HyperlinkId.eq(hyperlink_id))
        .filter(hyperlink_tag::Column::Source.eq(HyperlinkTagSource::Ai))
        .exec(&txn)
        .await?;

    for ranked in &ranked_tags {
        let tag_id = ensure_tag_with_state(&txn, ranked.tag.as_str(), ranked.state_if_new).await?;
        let existing = hyperlink_tag::Entity::find()
            .filter(hyperlink_tag::Column::HyperlinkId.eq(hyperlink_id))
            .filter(hyperlink_tag::Column::TagId.eq(tag_id))
            .one(&txn)
            .await?;

        if existing.is_some() {
            continue;
        }

        insert_hyperlink_tag(&txn, hyperlink_id, tag_id, HyperlinkTagSource::Ai, now).await?;
    }

    txn.commit().await?;

    let artifact_ranked_tags = ranked_tags
        .iter()
        .map(|ranked| RankedTag {
            tag: ranked.tag.clone(),
            confidence: ranked.confidence,
        })
        .collect::<Vec<_>>();
    let primary_tag = artifact_ranked_tags.first().map(|value| value.tag.clone());
    let meta = LegacyTagMeta {
        source: LegacyTaggingSource::Llm,
        ranked_tags: artifact_ranked_tags,
        primary_tag,
        overall_confidence: input.overall_confidence,
        rationale: input.rationale,
        provider: Some(input.provider),
        model: Some(input.model),
        prompt_version: Some(input.prompt_version),
        manual_override: false,
        classified_at: input.classified_at,
    };

    persist_tag_meta(connection, hyperlink_id, job_id, &meta).await
}

pub fn normalize_ranked_tags_for_vocabulary(
    ranked_tags: Vec<RankedTag>,
    vocabulary: &[String],
    minimum_confidence: f32,
) -> Vec<RankedTag> {
    let mut canonical_by_lower = HashMap::new();
    for value in vocabulary {
        canonical_by_lower.insert(value.to_ascii_lowercase(), value.to_string());
    }

    let mut scores = HashMap::<String, f32>::new();
    for ranked in ranked_tags {
        let key = ranked.tag.to_ascii_lowercase();
        let Some(canonical) = canonical_by_lower.get(&key) else {
            continue;
        };

        let confidence = ranked.confidence.clamp(0.0, 1.0);
        if confidence < minimum_confidence {
            continue;
        }

        let entry = scores.entry(canonical.clone()).or_insert(confidence);
        if confidence > *entry {
            *entry = confidence;
        }
    }

    let mut normalized = scores
        .into_iter()
        .map(|(tag, confidence)| RankedTag { tag, confidence })
        .collect::<Vec<_>>();
    normalized.sort_by(|left, right| {
        right
            .confidence
            .partial_cmp(&left.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.tag.cmp(&right.tag))
    });
    normalized
}

pub fn normalize_ranked_tags_with_discovery(
    ranked_tags: Vec<RankedTag>,
    vocabulary: &[String],
    minimum_confidence: f32,
) -> Vec<PersistedRankedTag> {
    let mut canonical_by_lower = HashMap::new();
    for value in vocabulary {
        canonical_by_lower.insert(value.to_ascii_lowercase(), value.to_string());
    }

    let mut ranked_by_key = HashMap::<String, PersistedRankedTag>::new();
    for ranked in ranked_tags {
        let confidence = ranked.confidence.clamp(0.0, 1.0);
        if confidence < minimum_confidence {
            continue;
        }

        let trimmed = ranked.tag.trim();
        if trimmed.is_empty() {
            continue;
        }

        let raw_key = trimmed.to_ascii_lowercase();
        let (canonical, state_if_new) = if let Some(existing) = canonical_by_lower.get(&raw_key) {
            (existing.clone(), TagState::AiApproved)
        } else {
            (trimmed.to_string(), TagState::AiPending)
        };
        let key = canonical.to_ascii_lowercase();

        match ranked_by_key.get_mut(&key) {
            Some(existing) => {
                if confidence > existing.confidence {
                    existing.confidence = confidence;
                    existing.tag = canonical;
                    existing.state_if_new = state_if_new;
                }
            }
            None => {
                ranked_by_key.insert(
                    key,
                    PersistedRankedTag {
                        tag: canonical,
                        confidence,
                        state_if_new,
                    },
                );
            }
        }
    }

    let mut normalized = ranked_by_key.into_values().collect::<Vec<_>>();
    normalized.sort_by(|left, right| {
        right
            .confidence
            .partial_cmp(&left.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.tag.cmp(&right.tag))
    });
    normalized
}

pub fn normalize_manual_tags_for_vocabulary(
    raw_tags: &[String],
    vocabulary: &[String],
) -> Vec<String> {
    if vocabulary.is_empty() {
        return dedupe_tags(raw_tags.to_vec());
    }

    let mut canonical_by_lower = HashMap::new();
    for value in vocabulary {
        canonical_by_lower.insert(value.to_ascii_lowercase(), value.to_string());
    }

    let mut normalized = Vec::new();
    let mut seen = HashSet::new();
    for raw in raw_tags {
        let key = raw.trim().to_ascii_lowercase();
        let Some(canonical) = canonical_by_lower.get(&key) else {
            continue;
        };

        if seen.insert(canonical.to_ascii_lowercase()) {
            normalized.push(canonical.clone());
        }
    }

    normalized
}

pub fn parse_manual_tags_input(raw: &str) -> Vec<String> {
    dedupe_tags(
        raw.lines()
            .flat_map(|line| line.split(','))
            .map(|token| token.trim().to_string())
            .collect(),
    )
}

fn dedupe_tags(values: Vec<String>) -> Vec<String> {
    let mut deduped = Vec::new();
    let mut seen = HashSet::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        let key = trimmed.to_ascii_lowercase();
        if seen.insert(key) {
            deduped.push(trimmed.to_string());
        }
    }
    deduped
}

fn dedupe_ranked_persisted_tags(values: Vec<PersistedRankedTag>) -> Vec<PersistedRankedTag> {
    let mut by_key = HashMap::<String, PersistedRankedTag>::new();
    for value in values {
        let key = value.tag.trim().to_ascii_lowercase();
        if key.is_empty() {
            continue;
        }

        match by_key.get_mut(&key) {
            Some(existing) => {
                if value.confidence > existing.confidence {
                    *existing = value;
                }
            }
            None => {
                by_key.insert(key, value);
            }
        }
    }

    let mut deduped = by_key.into_values().collect::<Vec<_>>();
    deduped.sort_by(|left, right| {
        right
            .confidence
            .partial_cmp(&left.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.tag.cmp(&right.tag))
    });
    deduped
}

async fn legacy_for_hyperlinks(
    connection: &DatabaseConnection,
    hyperlink_ids: &[i32],
) -> Result<HashMap<i32, TagSet>, DbErr> {
    let artifacts = hyperlink_artifact::latest_for_hyperlinks_kind(
        connection,
        hyperlink_ids,
        HyperlinkArtifactKind::TagMeta,
    )
    .await?;

    let mut tags_by_hyperlink = HashMap::with_capacity(artifacts.len());
    for (hyperlink_id, artifact) in artifacts {
        if let Ok(meta) = parse_artifact_payload(&artifact).await {
            tags_by_hyperlink.insert(hyperlink_id, legacy_tag_meta_to_set(meta));
        } else {
            tracing::warn!(
                artifact_id = artifact.id,
                hyperlink_id,
                "failed to parse legacy tag_meta artifact payload"
            );
        }
    }

    Ok(tags_by_hyperlink)
}

fn legacy_tag_meta_to_set(meta: LegacyTagMeta) -> TagSet {
    let source = match meta.source {
        LegacyTaggingSource::Manual => TaggingSource::User,
        LegacyTaggingSource::Llm => TaggingSource::Ai,
    };
    let state = match meta.source {
        LegacyTaggingSource::Manual => TagState::User,
        LegacyTaggingSource::Llm => TagState::AiApproved,
    };

    let mut all_tags = meta
        .ranked_tags
        .iter()
        .map(|ranked| LinkTag {
            tag: ranked.tag.clone(),
            source,
            state,
        })
        .collect::<Vec<_>>();
    normalize_link_tags(&mut all_tags);

    let mut set = build_tag_set(all_tags);
    if set.primary_visible_tag.is_none() {
        set.primary_visible_tag = meta.primary_tag;
    }
    set
}

fn normalize_link_tags(tags: &mut Vec<LinkTag>) {
    let mut seen = HashSet::new();
    tags.retain(|tag| seen.insert(tag.tag.to_ascii_lowercase()));
    tags.sort_by(|left, right| {
        source_rank(left.source)
            .cmp(&source_rank(right.source))
            .then_with(|| left.tag.cmp(&right.tag))
    });
}

fn build_tag_set(all_tags: Vec<LinkTag>) -> TagSet {
    let visible_tags = all_tags
        .iter()
        .filter(|entry| is_visible_state(entry.state))
        .map(|entry| entry.tag.clone())
        .collect::<Vec<_>>();
    let user_tags = all_tags
        .iter()
        .filter(|entry| entry.source == TaggingSource::User)
        .map(|entry| entry.tag.clone())
        .collect::<Vec<_>>();
    let primary_visible_tag = visible_tags.first().cloned();

    TagSet {
        all_tags,
        visible_tags,
        user_tags,
        primary_visible_tag,
    }
}

fn source_rank(source: TaggingSource) -> u8 {
    match source {
        TaggingSource::User => 0,
        TaggingSource::Ai => 1,
    }
}

fn is_visible_state(state: TagState) -> bool {
    matches!(state, TagState::User | TagState::AiApproved)
}

async fn ensure_tag_with_state(
    connection: &impl ConnectionTrait,
    raw_name: &str,
    state_if_missing: TagState,
) -> Result<i32, DbErr> {
    let (name, name_key) = normalize_tag_name(raw_name);
    if name_key.is_empty() {
        return Err(DbErr::Custom("tag name is empty".to_string()));
    }

    if let Some(existing) = tag::Entity::find()
        .filter(tag::Column::NameKey.eq(name_key.clone()))
        .one(connection)
        .await?
    {
        if state_if_missing == TagState::User && existing.state != tag::TagState::User {
            let mut active: tag::ActiveModel = existing.clone().into();
            active.state = Set(tag::TagState::User);
            active.updated_at = Set(now_utc());
            active.update(connection).await?;
        }
        return Ok(existing.id);
    }

    let now = now_utc();
    let inserted = tag::ActiveModel {
        name: Set(name),
        name_key: Set(name_key),
        state: Set(map_state_to_model(state_if_missing)),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(connection)
    .await?;

    Ok(inserted.id)
}

async fn insert_hyperlink_tag(
    connection: &impl ConnectionTrait,
    hyperlink_id: i32,
    tag_id: i32,
    source: HyperlinkTagSource,
    now: sea_orm::entity::prelude::DateTime,
) -> Result<(), DbErr> {
    hyperlink_tag::ActiveModel {
        hyperlink_id: Set(hyperlink_id),
        tag_id: Set(tag_id),
        source: Set(source),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(connection)
    .await
    .map(|_| ())
}

fn normalize_tag_name(raw: &str) -> (String, String) {
    let normalized = raw.trim().to_string();
    (normalized.clone(), normalized.to_ascii_lowercase())
}

fn map_state_from_model(state: tag::TagState) -> TagState {
    match state {
        tag::TagState::User => TagState::User,
        tag::TagState::AiApproved => TagState::AiApproved,
        tag::TagState::AiPending => TagState::AiPending,
        tag::TagState::AiRejected => TagState::AiRejected,
    }
}

fn map_state_to_model(state: TagState) -> tag::TagState {
    match state {
        TagState::User => tag::TagState::User,
        TagState::AiApproved => tag::TagState::AiApproved,
        TagState::AiPending => tag::TagState::AiPending,
        TagState::AiRejected => tag::TagState::AiRejected,
    }
}

fn map_source_from_model(source: HyperlinkTagSource) -> TaggingSource {
    match source {
        HyperlinkTagSource::User => TaggingSource::User,
        HyperlinkTagSource::Ai => TaggingSource::Ai,
    }
}

fn dedupe_i32(values: &[i32]) -> Vec<i32> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for value in values {
        if seen.insert(*value) {
            deduped.push(*value);
        }
    }
    deduped
}

fn now_utc() -> sea_orm::entity::prelude::DateTime {
    sea_orm::entity::prelude::DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}

async fn persist_tag_meta(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    job_id: Option<i32>,
    meta: &LegacyTagMeta,
) -> Result<(), DbErr> {
    let payload = serde_json::to_vec_pretty(meta)
        .map_err(|error| DbErr::Custom(format!("failed to encode tag_meta payload: {error}")))?;

    hyperlink_artifact::insert(
        connection,
        hyperlink_id,
        job_id,
        HyperlinkArtifactKind::TagMeta,
        payload,
        TAG_META_CONTENT_TYPE,
    )
    .await
    .map(|_| ())
}

async fn parse_artifact_payload(
    artifact: &crate::entity::hyperlink_artifact::Model,
) -> Result<LegacyTagMeta, DbErr> {
    let payload = hyperlink_artifact::load_processing_payload(artifact).await?;
    serde_json::from_slice::<LegacyTagMeta>(&payload).map_err(|error| {
        DbErr::Custom(format!(
            "failed to parse tag_meta artifact {}: {error}",
            artifact.id
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_ranked_tags_filters_unknown_and_sorts() {
        let normalized = normalize_ranked_tags_for_vocabulary(
            vec![
                RankedTag {
                    tag: "learn".to_string(),
                    confidence: 0.2,
                },
                RankedTag {
                    tag: "LEARN".to_string(),
                    confidence: 0.8,
                },
                RankedTag {
                    tag: "reference".to_string(),
                    confidence: 0.7,
                },
                RankedTag {
                    tag: "unknown".to_string(),
                    confidence: 0.9,
                },
            ],
            &["learn".to_string(), "reference".to_string()],
            0.3,
        );

        assert_eq!(
            normalized,
            vec![
                RankedTag {
                    tag: "learn".to_string(),
                    confidence: 0.8
                },
                RankedTag {
                    tag: "reference".to_string(),
                    confidence: 0.7
                }
            ]
        );
    }

    #[test]
    fn normalize_ranked_tags_with_discovery_marks_new_tags_pending() {
        let normalized = normalize_ranked_tags_with_discovery(
            vec![
                RankedTag {
                    tag: "learn".to_string(),
                    confidence: 0.9,
                },
                RankedTag {
                    tag: "Novel".to_string(),
                    confidence: 0.8,
                },
            ],
            &["learn".to_string()],
            0.3,
        );

        assert_eq!(normalized.len(), 2);
        assert_eq!(normalized[0].tag, "learn");
        assert_eq!(normalized[0].state_if_new, TagState::AiApproved);
        assert_eq!(normalized[1].tag, "Novel");
        assert_eq!(normalized[1].state_if_new, TagState::AiPending);
    }

    #[test]
    fn parse_manual_tags_input_supports_commas_and_lines() {
        let parsed = parse_manual_tags_input("learn, build\nreference\nlearn");
        assert_eq!(
            parsed,
            vec![
                "learn".to_string(),
                "build".to_string(),
                "reference".to_string()
            ]
        );
    }
}
