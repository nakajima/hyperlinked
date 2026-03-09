use std::collections::{HashMap, HashSet};

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr,
    EntityTrait, QueryFilter, QueryOrder, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};

use crate::{
    entity::{
        action_tag,
        hyperlink_action_tag::{self, HyperlinkActionTagSource},
        hyperlink_artifact::HyperlinkArtifactKind,
        hyperlink_topic_tag::{self, HyperlinkTopicTagSource},
        topic_tag,
    },
    model::hyperlink_artifact,
};

pub const TAG_META_CONTENT_TYPE: &str = "application/json";
pub const TAGGING_PROMPT_VERSION: &str = "v2";

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TagKind {
    Topic,
    Action,
}

impl TagKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Topic => "topic",
            Self::Action => "action",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LinkTag {
    pub tag: String,
    pub source: TaggingSource,
    pub state: TagState,
    pub confidence: f32,
    pub rank_index: i32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TagSet {
    pub all_tags: Vec<LinkTag>,
    pub visible_tags: Vec<String>,
    pub user_tags: Vec<String>,
    pub primary_visible_tag: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingTag {
    pub kind: TagKind,
    pub id: i32,
    pub name: String,
    pub hyperlink_count: usize,
    pub encoded_id: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ApprovedPendingTags {
    pub topic_names: Vec<String>,
    pub action_names: Vec<String>,
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
    #[serde(default)]
    ranked_tags: Vec<RankedTag>,
    #[serde(default)]
    topic_ranked_tags: Vec<RankedTag>,
    #[serde(default)]
    action_ranked_tags: Vec<RankedTag>,
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
    pub topic_ranked_tags: Vec<PersistedRankedTag>,
    pub action_ranked_tags: Vec<PersistedRankedTag>,
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

    let rows = hyperlink_topic_tag::Entity::find()
        .filter(hyperlink_topic_tag::Column::HyperlinkId.is_in(hyperlink_ids.to_vec()))
        .find_also_related(topic_tag::Entity)
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
                source: map_topic_source_from_model(link_tag.source),
                state: map_topic_state_from_model(model.state),
                confidence: link_tag.confidence,
                rank_index: link_tag.rank_index,
            });
    }

    let mut sets = HashMap::with_capacity(by_hyperlink.len());
    for (hyperlink_id, mut tags) in by_hyperlink {
        normalize_link_tags(&mut tags);
        sets.insert(hyperlink_id, build_tag_set(tags));
    }

    Ok(sets)
}

pub async fn list_pending_tags(connection: &DatabaseConnection) -> Result<Vec<PendingTag>, DbErr> {
    let backend = connection.get_database_backend();

    let topic_rows = connection
        .query_all(Statement::from_string(
            backend,
            r#"
                SELECT
                    t.id AS id,
                    t.name AS name,
                    COUNT(ht.id) AS hyperlink_count
                FROM topic_tag t
                LEFT JOIN hyperlink_topic_tag ht
                    ON ht.topic_tag_id = t.id
                WHERE t.state = 'AI_PENDING'
                GROUP BY t.id, t.name
                ORDER BY t.name ASC
            "#
            .to_string(),
        ))
        .await?;

    let action_rows = connection
        .query_all(Statement::from_string(
            backend,
            r#"
                SELECT
                    t.id AS id,
                    t.name AS name,
                    COUNT(ht.id) AS hyperlink_count
                FROM action_tag t
                LEFT JOIN hyperlink_action_tag ht
                    ON ht.action_tag_id = t.id
                WHERE t.state = 'AI_PENDING'
                GROUP BY t.id, t.name
                ORDER BY t.name ASC
            "#
            .to_string(),
        ))
        .await?;

    let mut pending = Vec::with_capacity(topic_rows.len() + action_rows.len());
    for row in topic_rows {
        let id: i32 = row.try_get("", "id")?;
        let name: String = row.try_get("", "name")?;
        let hyperlink_count: i64 = row.try_get("", "hyperlink_count")?;
        pending.push(PendingTag {
            kind: TagKind::Topic,
            id,
            name,
            hyperlink_count: hyperlink_count.max(0) as usize,
            encoded_id: format!("topic:{id}"),
        });
    }

    for row in action_rows {
        let id: i32 = row.try_get("", "id")?;
        let name: String = row.try_get("", "name")?;
        let hyperlink_count: i64 = row.try_get("", "hyperlink_count")?;
        pending.push(PendingTag {
            kind: TagKind::Action,
            id,
            name,
            hyperlink_count: hyperlink_count.max(0) as usize,
            encoded_id: format!("action:{id}"),
        });
    }

    pending.sort_by(|left, right| {
        tag_kind_rank(left.kind)
            .cmp(&tag_kind_rank(right.kind))
            .then_with(|| left.name.cmp(&right.name))
    });

    Ok(pending)
}

pub async fn approve_pending_tags(
    connection: &DatabaseConnection,
    encoded_ids: &[String],
) -> Result<ApprovedPendingTags, DbErr> {
    if encoded_ids.is_empty() {
        return Ok(ApprovedPendingTags::default());
    }

    let mut topic_ids = Vec::new();
    let mut action_ids = Vec::new();
    let mut seen = HashSet::new();
    for encoded in encoded_ids {
        let Some((kind, raw_id)) = encoded.split_once(':') else {
            continue;
        };
        let Ok(id) = raw_id.parse::<i32>() else {
            continue;
        };
        let dedupe_key = format!("{kind}:{id}");
        if !seen.insert(dedupe_key) {
            continue;
        }

        match kind {
            "topic" => topic_ids.push(id),
            "action" => action_ids.push(id),
            _ => {}
        }
    }

    let mut approved = ApprovedPendingTags::default();
    let now = now_utc();

    if !topic_ids.is_empty() {
        let rows = topic_tag::Entity::find()
            .filter(topic_tag::Column::Id.is_in(topic_ids.clone()))
            .filter(topic_tag::Column::State.eq(topic_tag::TopicTagState::AiPending))
            .order_by_asc(topic_tag::Column::Name)
            .all(connection)
            .await?;

        approved.topic_names = rows.into_iter().map(|row| row.name).collect();

        topic_tag::Entity::update_many()
            .col_expr(
                topic_tag::Column::State,
                sea_orm::sea_query::Expr::value(topic_tag::TopicTagState::AiApproved),
            )
            .col_expr(
                topic_tag::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(now),
            )
            .filter(topic_tag::Column::Id.is_in(topic_ids))
            .filter(topic_tag::Column::State.eq(topic_tag::TopicTagState::AiPending))
            .exec(connection)
            .await?;
    }

    if !action_ids.is_empty() {
        let rows = action_tag::Entity::find()
            .filter(action_tag::Column::Id.is_in(action_ids.clone()))
            .filter(action_tag::Column::State.eq(action_tag::ActionTagState::AiPending))
            .order_by_asc(action_tag::Column::Name)
            .all(connection)
            .await?;

        approved.action_names = rows.into_iter().map(|row| row.name).collect();

        action_tag::Entity::update_many()
            .col_expr(
                action_tag::Column::State,
                sea_orm::sea_query::Expr::value(action_tag::ActionTagState::AiApproved),
            )
            .col_expr(
                action_tag::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(now),
            )
            .filter(action_tag::Column::Id.is_in(action_ids))
            .filter(action_tag::Column::State.eq(action_tag::ActionTagState::AiPending))
            .exec(connection)
            .await?;
    }

    Ok(approved)
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
    hyperlink_topic_tag::Entity::delete_many()
        .filter(hyperlink_topic_tag::Column::HyperlinkId.eq(hyperlink_id))
        .exec(&txn)
        .await?;

    for (index, tag_name) in tags.iter().enumerate() {
        let tag_id = ensure_topic_tag_with_state(&txn, tag_name, TagState::User).await?;
        insert_hyperlink_topic_tag(
            &txn,
            hyperlink_id,
            tag_id,
            HyperlinkTopicTagSource::User,
            (1.0 - (index as f32 * 0.1)).max(0.0),
            index as i32,
            now_utc(),
        )
        .await?;
    }

    txn.commit().await?;

    let primary_tag = ranked_tags.first().map(|value| value.tag.clone());
    let meta = LegacyTagMeta {
        source: LegacyTaggingSource::Manual,
        ranked_tags: ranked_tags.clone(),
        topic_ranked_tags: ranked_tags,
        action_ranked_tags: Vec::new(),
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
    let topic_ranked_tags = dedupe_ranked_persisted_tags(input.topic_ranked_tags);
    let action_ranked_tags = dedupe_ranked_persisted_tags(input.action_ranked_tags);
    let now = now_utc();

    let txn = connection.begin().await?;
    hyperlink_topic_tag::Entity::delete_many()
        .filter(hyperlink_topic_tag::Column::HyperlinkId.eq(hyperlink_id))
        .filter(hyperlink_topic_tag::Column::Source.eq(HyperlinkTopicTagSource::Ai))
        .exec(&txn)
        .await?;
    hyperlink_action_tag::Entity::delete_many()
        .filter(hyperlink_action_tag::Column::HyperlinkId.eq(hyperlink_id))
        .filter(hyperlink_action_tag::Column::Source.eq(HyperlinkActionTagSource::Ai))
        .exec(&txn)
        .await?;

    for (index, ranked) in topic_ranked_tags.iter().enumerate() {
        let tag_id =
            ensure_topic_tag_with_state(&txn, ranked.tag.as_str(), ranked.state_if_new).await?;
        let existing = hyperlink_topic_tag::Entity::find()
            .filter(hyperlink_topic_tag::Column::HyperlinkId.eq(hyperlink_id))
            .filter(hyperlink_topic_tag::Column::TopicTagId.eq(tag_id))
            .one(&txn)
            .await?;

        if existing.is_some() {
            continue;
        }

        insert_hyperlink_topic_tag(
            &txn,
            hyperlink_id,
            tag_id,
            HyperlinkTopicTagSource::Ai,
            ranked.confidence,
            index as i32,
            now,
        )
        .await?;
    }

    for (index, ranked) in action_ranked_tags.iter().enumerate() {
        let tag_id =
            ensure_action_tag_with_state(&txn, ranked.tag.as_str(), ranked.state_if_new).await?;
        let existing = hyperlink_action_tag::Entity::find()
            .filter(hyperlink_action_tag::Column::HyperlinkId.eq(hyperlink_id))
            .filter(hyperlink_action_tag::Column::ActionTagId.eq(tag_id))
            .one(&txn)
            .await?;

        if existing.is_some() {
            continue;
        }

        insert_hyperlink_action_tag(
            &txn,
            hyperlink_id,
            tag_id,
            HyperlinkActionTagSource::Ai,
            ranked.confidence,
            index as i32,
            now,
        )
        .await?;
    }

    txn.commit().await?;

    let artifact_topic_tags = topic_ranked_tags
        .iter()
        .map(|ranked| RankedTag {
            tag: ranked.tag.clone(),
            confidence: ranked.confidence,
        })
        .collect::<Vec<_>>();
    let artifact_action_tags = action_ranked_tags
        .iter()
        .map(|ranked| RankedTag {
            tag: ranked.tag.clone(),
            confidence: ranked.confidence,
        })
        .collect::<Vec<_>>();
    let primary_tag = artifact_topic_tags.first().map(|value| value.tag.clone());
    let meta = LegacyTagMeta {
        source: LegacyTaggingSource::Llm,
        ranked_tags: artifact_topic_tags.clone(),
        topic_ranked_tags: artifact_topic_tags,
        action_ranked_tags: artifact_action_tags,
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
        let canonical = value.trim().to_ascii_lowercase();
        if !canonical.is_empty() {
            canonical_by_lower.insert(canonical.clone(), canonical);
        }
    }

    let mut scores = HashMap::<String, f32>::new();
    for ranked in ranked_tags {
        let key = ranked.tag.trim().to_ascii_lowercase();
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
    auto_approve_ai: bool,
) -> Vec<PersistedRankedTag> {
    let mut canonical_by_lower = HashMap::new();
    for value in vocabulary {
        let canonical = value.trim().to_ascii_lowercase();
        if !canonical.is_empty() {
            canonical_by_lower.insert(canonical.clone(), canonical);
        }
    }

    let mut ranked_by_key = HashMap::<String, PersistedRankedTag>::new();
    for ranked in ranked_tags {
        let confidence = ranked.confidence.clamp(0.0, 1.0);
        if confidence < minimum_confidence {
            continue;
        }

        let trimmed = ranked.tag.trim().to_ascii_lowercase();
        if !is_tag_candidate(trimmed.as_str()) {
            continue;
        }

        let state_if_new = if auto_approve_ai {
            TagState::AiApproved
        } else {
            TagState::AiPending
        };
        let canonical = canonical_by_lower.get(&trimmed).cloned().unwrap_or(trimmed);
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
        let canonical = value.trim().to_ascii_lowercase();
        if !canonical.is_empty() {
            canonical_by_lower.insert(canonical.clone(), canonical);
        }
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
        let trimmed = value.trim().to_ascii_lowercase();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.clone()) {
            deduped.push(trimmed);
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
                    *existing = PersistedRankedTag {
                        tag: key.clone(),
                        confidence: value.confidence,
                        state_if_new: value.state_if_new,
                    };
                }
            }
            None => {
                by_key.insert(
                    key.clone(),
                    PersistedRankedTag {
                        tag: key,
                        confidence: value.confidence,
                        state_if_new: value.state_if_new,
                    },
                );
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

fn normalize_link_tags(tags: &mut Vec<LinkTag>) {
    let mut seen = HashSet::new();
    tags.retain(|tag| {
        let key = tag.tag.trim().to_ascii_lowercase();
        if key.is_empty() {
            return false;
        }
        seen.insert(key)
    });
    tags.sort_by(|left, right| {
        source_rank(left.source)
            .cmp(&source_rank(right.source))
            .then_with(|| left.rank_index.cmp(&right.rank_index))
            .then_with(|| {
                right
                    .confidence
                    .partial_cmp(&left.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| left.tag.cmp(&right.tag))
    });
}

fn build_tag_set(all_tags: Vec<LinkTag>) -> TagSet {
    let visible_tags = all_tags
        .iter()
        .filter(|entry| is_visible_state(entry.state))
        .map(|entry| entry.tag.clone())
        .take(5)
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

fn tag_kind_rank(kind: TagKind) -> u8 {
    match kind {
        TagKind::Topic => 0,
        TagKind::Action => 1,
    }
}

fn is_visible_state(state: TagState) -> bool {
    matches!(state, TagState::User | TagState::AiApproved)
}

async fn ensure_topic_tag_with_state(
    connection: &impl ConnectionTrait,
    raw_name: &str,
    state_if_missing: TagState,
) -> Result<i32, DbErr> {
    let (name, name_key) = normalize_tag_name(raw_name);
    if name_key.is_empty() {
        return Err(DbErr::Custom("topic tag name is empty".to_string()));
    }

    if let Some(existing) = topic_tag::Entity::find()
        .filter(topic_tag::Column::NameKey.eq(name_key.clone()))
        .one(connection)
        .await?
    {
        if state_if_missing == TagState::User && existing.state != topic_tag::TopicTagState::User {
            let mut active: topic_tag::ActiveModel = existing.clone().into();
            active.state = Set(topic_tag::TopicTagState::User);
            active.updated_at = Set(now_utc());
            active.update(connection).await?;
        }
        return Ok(existing.id);
    }

    let now = now_utc();
    let inserted = topic_tag::ActiveModel {
        name: Set(name),
        name_key: Set(name_key),
        state: Set(map_topic_state_to_model(state_if_missing)),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(connection)
    .await?;

    Ok(inserted.id)
}

async fn ensure_action_tag_with_state(
    connection: &impl ConnectionTrait,
    raw_name: &str,
    state_if_missing: TagState,
) -> Result<i32, DbErr> {
    let (name, name_key) = normalize_tag_name(raw_name);
    if name_key.is_empty() {
        return Err(DbErr::Custom("action tag name is empty".to_string()));
    }

    if let Some(existing) = action_tag::Entity::find()
        .filter(action_tag::Column::NameKey.eq(name_key.clone()))
        .one(connection)
        .await?
    {
        if state_if_missing == TagState::User && existing.state != action_tag::ActionTagState::User
        {
            let mut active: action_tag::ActiveModel = existing.clone().into();
            active.state = Set(action_tag::ActionTagState::User);
            active.updated_at = Set(now_utc());
            active.update(connection).await?;
        }
        return Ok(existing.id);
    }

    let now = now_utc();
    let inserted = action_tag::ActiveModel {
        name: Set(name),
        name_key: Set(name_key),
        state: Set(map_action_state_to_model(state_if_missing)),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(connection)
    .await?;

    Ok(inserted.id)
}

async fn insert_hyperlink_topic_tag(
    connection: &impl ConnectionTrait,
    hyperlink_id: i32,
    topic_tag_id: i32,
    source: HyperlinkTopicTagSource,
    confidence: f32,
    rank_index: i32,
    now: sea_orm::entity::prelude::DateTime,
) -> Result<(), DbErr> {
    hyperlink_topic_tag::ActiveModel {
        hyperlink_id: Set(hyperlink_id),
        topic_tag_id: Set(topic_tag_id),
        source: Set(source),
        confidence: Set(confidence.clamp(0.0, 1.0)),
        rank_index: Set(rank_index.max(0)),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(connection)
    .await
    .map(|_| ())
}

async fn insert_hyperlink_action_tag(
    connection: &impl ConnectionTrait,
    hyperlink_id: i32,
    action_tag_id: i32,
    source: HyperlinkActionTagSource,
    confidence: f32,
    rank_index: i32,
    now: sea_orm::entity::prelude::DateTime,
) -> Result<(), DbErr> {
    hyperlink_action_tag::ActiveModel {
        hyperlink_id: Set(hyperlink_id),
        action_tag_id: Set(action_tag_id),
        source: Set(source),
        confidence: Set(confidence.clamp(0.0, 1.0)),
        rank_index: Set(rank_index.max(0)),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(connection)
    .await
    .map(|_| ())
}

fn normalize_tag_name(raw: &str) -> (String, String) {
    let normalized = raw.trim().to_ascii_lowercase();
    (normalized.clone(), normalized)
}

fn map_topic_state_from_model(state: topic_tag::TopicTagState) -> TagState {
    match state {
        topic_tag::TopicTagState::User => TagState::User,
        topic_tag::TopicTagState::AiApproved => TagState::AiApproved,
        topic_tag::TopicTagState::AiPending => TagState::AiPending,
        topic_tag::TopicTagState::AiRejected => TagState::AiRejected,
    }
}

fn map_action_state_to_model(state: TagState) -> action_tag::ActionTagState {
    match state {
        TagState::User => action_tag::ActionTagState::User,
        TagState::AiApproved => action_tag::ActionTagState::AiApproved,
        TagState::AiPending => action_tag::ActionTagState::AiPending,
        TagState::AiRejected => action_tag::ActionTagState::AiRejected,
    }
}

fn map_topic_state_to_model(state: TagState) -> topic_tag::TopicTagState {
    match state {
        TagState::User => topic_tag::TopicTagState::User,
        TagState::AiApproved => topic_tag::TopicTagState::AiApproved,
        TagState::AiPending => topic_tag::TopicTagState::AiPending,
        TagState::AiRejected => topic_tag::TopicTagState::AiRejected,
    }
}

fn map_topic_source_from_model(source: HyperlinkTopicTagSource) -> TaggingSource {
    match source {
        HyperlinkTopicTagSource::User => TaggingSource::User,
        HyperlinkTopicTagSource::Ai => TaggingSource::Ai,
    }
}

fn now_utc() -> sea_orm::entity::prelude::DateTime {
    sea_orm::entity::prelude::DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}

fn is_tag_candidate(value: &str) -> bool {
    if value.is_empty() || value.len() > 48 {
        return false;
    }

    if value.split_whitespace().count() > 4 {
        return false;
    }

    let mut has_alpha = false;
    for ch in value.chars() {
        if ch.is_ascii_alphabetic() {
            has_alpha = true;
            continue;
        }
        if ch.is_ascii_digit() || ch == ' ' || ch == '-' || ch == '&' || ch == '/' {
            continue;
        }
        return false;
    }

    has_alpha
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_ranked_tags_filters_unknown_and_sorts() {
        let normalized = normalize_ranked_tags_for_vocabulary(
            vec![
                RankedTag {
                    tag: "Typography".to_string(),
                    confidence: 0.9,
                },
                RankedTag {
                    tag: "TYPOGRAPHY".to_string(),
                    confidence: 0.8,
                },
                RankedTag {
                    tag: "bad".to_string(),
                    confidence: 0.95,
                },
            ],
            &["typography".to_string()],
            0.35,
        );

        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized[0].tag, "typography");
        assert_eq!(normalized[0].confidence, 0.9);
    }

    #[test]
    fn normalize_ranked_tags_with_discovery_marks_new_tags_pending() {
        let normalized = normalize_ranked_tags_with_discovery(
            vec![
                RankedTag {
                    tag: "Type Foundry".to_string(),
                    confidence: 0.8,
                },
                RankedTag {
                    tag: "Typ0?".to_string(),
                    confidence: 0.7,
                },
            ],
            &["typography".to_string()],
            0.35,
            false,
        );

        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized[0].tag, "type foundry");
        assert_eq!(normalized[0].state_if_new, TagState::AiPending);
    }

    #[test]
    fn parse_manual_tags_input_supports_commas_and_lines() {
        let parsed = parse_manual_tags_input("Typography, Tooling\nreference\nTypography");
        assert_eq!(
            parsed,
            vec![
                "typography".to_string(),
                "tooling".to_string(),
                "reference".to_string()
            ]
        );
    }

    #[test]
    fn is_tag_candidate_rejects_bad_tokens() {
        assert!(is_tag_candidate("type foundry"));
        assert!(!is_tag_candidate(""));
        assert!(!is_tag_candidate("!!!!!"));
        assert!(!is_tag_candidate("this has too many words in tag name now"));
    }
}
