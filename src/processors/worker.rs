use sea_orm::{
    ActiveModelTrait,
    ActiveValue::Set,
    DatabaseConnection, EntityTrait,
    entity::prelude::{DateTime, DateTimeUtc},
};

use crate::{
    app::models::{
        artifact_job::{self, ArtifactFetchMode, ArtifactJobResolveResult},
        hyperlink::ROOT_DISCOVERY_DEPTH,
        hyperlink_processing_job::{self as hyperlink_processing_job_model, ProcessingQueueSender},
        settings,
    },
    entity::{
        hyperlink,
        hyperlink_processing_job::{self, HyperlinkProcessingJobKind, HyperlinkProcessingJobState},
    },
    processors::pipeline::Pipeline,
};

pub async fn process_job(
    connection: &DatabaseConnection,
    sender: &ProcessingQueueSender,
    job_id: i32,
) -> Result<(), sea_orm::DbErr> {
    let Some(model) = hyperlink_processing_job::Entity::find_by_id(job_id)
        .one(connection)
        .await?
    else {
        return Ok(());
    };

    if matches!(
        model.state,
        HyperlinkProcessingJobState::Succeeded | HyperlinkProcessingJobState::Failed
    ) {
        return Ok(());
    }

    let now = now_utc();
    let started_at = model.started_at.or(Some(now));
    let mut job_active_model: hyperlink_processing_job::ActiveModel = model.into();
    job_active_model.state = Set(HyperlinkProcessingJobState::Running);
    job_active_model.started_at = Set(started_at);
    job_active_model.finished_at = Set(None);
    job_active_model.error_message = Set(None);
    job_active_model.updated_at = Set(now);
    let running_job = job_active_model.update(connection).await?;

    let Some(hyperlink_model) = hyperlink::Entity::find_by_id(running_job.hyperlink_id)
        .one(connection)
        .await?
    else {
        mark_job_failed(
            connection,
            running_job.id,
            "hyperlink does not exist for queued processing job",
        )
        .await?;
        return Ok(());
    };

    let hyperlink_discovery_depth = hyperlink_model.discovery_depth;
    let mut hyperlink_active_model: hyperlink::ActiveModel = hyperlink_model.into();
    let mut pipeline = Pipeline::new(
        &mut hyperlink_active_model,
        running_job.id,
        Some(sender.clone()),
    );
    let collection_settings = settings::load(connection).await?;

    if !collection_settings.allows_processing_job_kind(running_job.kind.clone()) {
        tracing::info!(
            hyperlink_id = running_job.hyperlink_id,
            job_id = running_job.id,
            kind = ?running_job.kind,
            "processing skipped because artifact collection for this job kind is disabled"
        );
        mark_job_succeeded(connection, running_job.id).await?;
        return Ok(());
    }

    match running_job.kind {
        HyperlinkProcessingJobKind::Snapshot => match pipeline.process_snapshot(connection).await {
            Ok(_) => {
                hyperlink_active_model.updated_at = Set(now_utc());
                hyperlink_active_model.update(connection).await?;
                mark_job_succeeded(connection, running_job.id).await?;
                if collection_settings.collect_og {
                    enqueue_followup_artifact_job_after_snapshot(
                        connection,
                        sender,
                        running_job.hyperlink_id,
                        HyperlinkProcessingJobKind::Og,
                        collection_settings,
                    )
                    .await?;
                }
                if collection_settings.collect_readability {
                    enqueue_followup_artifact_job_after_snapshot(
                        connection,
                        sender,
                        running_job.hyperlink_id,
                        HyperlinkProcessingJobKind::Readability,
                        collection_settings,
                    )
                    .await?;
                }
            }
            Err(error) => {
                mark_job_failed(connection, running_job.id, &error.to_string()).await?;
                tracing::warn!(
                    hyperlink_id = running_job.hyperlink_id,
                    job_id = running_job.id,
                    kind = "snapshot",
                    error = %error,
                    "hyperlink processing job failed"
                );
            }
        },
        HyperlinkProcessingJobKind::Readability => {
            match pipeline.process_readability(connection).await {
                Ok(_) => {
                    if hyperlink_active_model.is_changed() {
                        hyperlink_active_model.updated_at = Set(now_utc());
                        hyperlink_active_model.update(connection).await?;
                    }
                    mark_job_succeeded(connection, running_job.id).await?;
                    if should_enqueue_sublink_discovery(hyperlink_discovery_depth) {
                        enqueue_sublink_discovery_job(connection, sender, running_job.hyperlink_id)
                            .await?;
                    }
                }
                Err(error) => {
                    mark_job_failed(connection, running_job.id, &error.to_string()).await?;
                    tracing::warn!(
                        hyperlink_id = running_job.hyperlink_id,
                        job_id = running_job.id,
                        kind = "readability",
                        error = %error,
                        "hyperlink processing job failed"
                    );
                }
            }
        }
        HyperlinkProcessingJobKind::Og => match pipeline.process_og(connection).await {
            Ok(_) => {
                hyperlink_active_model.updated_at = Set(now_utc());
                hyperlink_active_model.update(connection).await?;
                mark_job_succeeded(connection, running_job.id).await?;
            }
            Err(error) => {
                mark_job_failed(connection, running_job.id, &error.to_string()).await?;
                tracing::warn!(
                    hyperlink_id = running_job.hyperlink_id,
                    job_id = running_job.id,
                    kind = "og",
                    error = %error,
                    "hyperlink processing job failed"
                );
            }
        },
        HyperlinkProcessingJobKind::Oembed => {
            tracing::info!(
                hyperlink_id = running_job.hyperlink_id,
                job_id = running_job.id,
                kind = "oembed",
                "oembed processing is disabled; marking legacy job as succeeded"
            );
            mark_job_succeeded(connection, running_job.id).await?;
        }
        HyperlinkProcessingJobKind::SublinkDiscovery => {
            match pipeline.process_sublink_discovery(connection).await {
                Ok(_) => {
                    mark_job_succeeded(connection, running_job.id).await?;
                }
                Err(error) => {
                    mark_job_failed(connection, running_job.id, &error.to_string()).await?;
                    tracing::warn!(
                        hyperlink_id = running_job.hyperlink_id,
                        job_id = running_job.id,
                        kind = "sublink_discovery",
                        error = %error,
                        "hyperlink processing job failed"
                    );
                }
            }
        }
    }

    Ok(())
}

async fn enqueue_followup_artifact_job_after_snapshot(
    connection: &DatabaseConnection,
    sender: &ProcessingQueueSender,
    hyperlink_id: i32,
    kind: HyperlinkProcessingJobKind,
    settings: settings::ArtifactCollectionSettings,
) -> Result<(), sea_orm::DbErr> {
    let result = artifact_job::resolve_and_enqueue_for_job_kind_with_settings(
        connection,
        hyperlink_id,
        kind.clone(),
        ArtifactFetchMode::RefetchTarget,
        settings,
        Some(sender),
    )
    .await?;

    match result {
        ArtifactJobResolveResult::EnqueuedRequested { .. }
        | ArtifactJobResolveResult::EnqueuedDependency { .. }
        | ArtifactJobResolveResult::AlreadySatisfied { .. }
        | ArtifactJobResolveResult::DisabledRequested { .. }
        | ArtifactJobResolveResult::DisabledDependency { .. }
        | ArtifactJobResolveResult::UnfetchableDependency { .. } => {}
        ArtifactJobResolveResult::UnsupportedArtifactKind { .. }
        | ArtifactJobResolveResult::UnsupportedJobKind { .. } => {
            return Err(sea_orm::DbErr::Custom(
                format!("unsupported artifact follow-up job kind: {kind:?}").into(),
            ));
        }
    }

    Ok(())
}

async fn enqueue_sublink_discovery_job(
    connection: &DatabaseConnection,
    sender: &ProcessingQueueSender,
    hyperlink_id: i32,
) -> Result<(), sea_orm::DbErr> {
    hyperlink_processing_job_model::enqueue_for_hyperlink_kind(
        connection,
        hyperlink_id,
        HyperlinkProcessingJobKind::SublinkDiscovery,
        Some(sender),
    )
    .await?;
    Ok(())
}

async fn mark_job_succeeded(
    connection: &DatabaseConnection,
    job_id: i32,
) -> Result<(), sea_orm::DbErr> {
    update_job_state(
        connection,
        job_id,
        HyperlinkProcessingJobState::Succeeded,
        None,
    )
    .await
}

async fn mark_job_failed(
    connection: &DatabaseConnection,
    job_id: i32,
    error_message: &str,
) -> Result<(), sea_orm::DbErr> {
    update_job_state(
        connection,
        job_id,
        HyperlinkProcessingJobState::Failed,
        Some(error_message.to_string()),
    )
    .await
}

async fn update_job_state(
    connection: &DatabaseConnection,
    job_id: i32,
    state: HyperlinkProcessingJobState,
    error_message: Option<String>,
) -> Result<(), sea_orm::DbErr> {
    let Some(model) = hyperlink_processing_job::Entity::find_by_id(job_id)
        .one(connection)
        .await?
    else {
        return Ok(());
    };

    let mut active_model: hyperlink_processing_job::ActiveModel = model.into();
    active_model.state = Set(state);
    active_model.finished_at = Set(Some(now_utc()));
    active_model.error_message = Set(error_message);
    active_model.updated_at = Set(now_utc());
    active_model.update(connection).await?;
    Ok(())
}

fn now_utc() -> DateTime {
    DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}

fn should_enqueue_sublink_discovery(discovery_depth: i32) -> bool {
    discovery_depth == ROOT_DISCOVERY_DEPTH
}
#[cfg(test)]
#[path = "../../tests/unit/processors_worker.rs"]
mod tests;
