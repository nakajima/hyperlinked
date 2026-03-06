use std::collections::{HashMap, HashSet};

use sea_orm::{DatabaseConnection, DbErr};
use serde::{Deserialize, Serialize};

use crate::{entity::hyperlink_artifact::HyperlinkArtifactKind, model::hyperlink_artifact};

pub const TAG_META_CONTENT_TYPE: &str = "application/json";
pub const TAGGING_PROMPT_VERSION: &str = "v1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RankedTag {
    pub tag: String,
    pub confidence: f32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TaggingSource {
    Llm,
    Manual,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TagMeta {
    pub source: TaggingSource,
    pub ranked_tags: Vec<RankedTag>,
    pub primary_tag: Option<String>,
    pub overall_confidence: Option<f32>,
    pub rationale: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub prompt_version: Option<String>,
    pub manual_override: bool,
    pub classified_at: String,
}

#[derive(Clone, Debug)]
pub struct LlmPersistInput {
    pub ranked_tags: Vec<RankedTag>,
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
) -> Result<Option<TagMeta>, DbErr> {
    let artifact = hyperlink_artifact::latest_for_hyperlink_kind(
        connection,
        hyperlink_id,
        HyperlinkArtifactKind::TagMeta,
    )
    .await?;

    let Some(artifact) = artifact else {
        return Ok(None);
    };

    parse_artifact_payload(&artifact).await.map(Some)
}

pub async fn latest_for_hyperlinks(
    connection: &DatabaseConnection,
    hyperlink_ids: &[i32],
) -> Result<HashMap<i32, TagMeta>, DbErr> {
    let artifacts = hyperlink_artifact::latest_for_hyperlinks_kind(
        connection,
        hyperlink_ids,
        HyperlinkArtifactKind::TagMeta,
    )
    .await?;

    let mut tags_by_hyperlink = HashMap::with_capacity(artifacts.len());
    for (hyperlink_id, artifact) in artifacts {
        if let Ok(meta) = parse_artifact_payload(&artifact).await {
            tags_by_hyperlink.insert(hyperlink_id, meta);
        } else {
            tracing::warn!(
                artifact_id = artifact.id,
                hyperlink_id,
                "failed to parse tag_meta artifact payload"
            );
        }
    }

    Ok(tags_by_hyperlink)
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
    let primary_tag = ranked_tags.first().map(|value| value.tag.clone());

    let meta = TagMeta {
        source: TaggingSource::Manual,
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
    job_id: i32,
    input: LlmPersistInput,
) -> Result<(), DbErr> {
    let primary_tag = input.ranked_tags.first().map(|value| value.tag.clone());
    let meta = TagMeta {
        source: TaggingSource::Llm,
        ranked_tags: input.ranked_tags,
        primary_tag,
        overall_confidence: input.overall_confidence,
        rationale: input.rationale,
        provider: Some(input.provider),
        model: Some(input.model),
        prompt_version: Some(input.prompt_version),
        manual_override: false,
        classified_at: input.classified_at,
    };

    persist_tag_meta(connection, hyperlink_id, Some(job_id), &meta).await
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

async fn persist_tag_meta(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    job_id: Option<i32>,
    meta: &TagMeta,
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
) -> Result<TagMeta, DbErr> {
    let payload = hyperlink_artifact::load_processing_payload(artifact).await?;
    serde_json::from_slice::<TagMeta>(&payload).map_err(|error| {
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
