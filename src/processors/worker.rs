use sea_orm::{
    ActiveModelTrait,
    ActiveValue::Set,
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QuerySelect,
    entity::prelude::{DateTime, DateTimeUtc},
};

use crate::{
    entity::hyperlink::{self, HyperlinkProcessingState},
    model::{hyperlink::ProcessingQueueSender, hyperlink_processing_error},
    processors::pipeline::Pipeline,
};

pub fn spawn(connection: DatabaseConnection) -> ProcessingQueueSender {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<i32>();

    let worker_connection = connection.clone();
    tokio::spawn(async move {
        while let Some(hyperlink_id) = receiver.recv().await {
            if let Err(error) = process_one(&worker_connection, hyperlink_id).await {
                tracing::error!(
                    hyperlink_id,
                    error = %error,
                    "failed to process hyperlink in background worker"
                );
            }
        }
    });

    let bootstrap_connection = connection;
    let bootstrap_sender = sender.clone();
    tokio::spawn(async move {
        if let Err(error) = enqueue_waiting(&bootstrap_connection, &bootstrap_sender).await {
            tracing::error!(error = %error, "failed to enqueue waiting hyperlinks");
        }
    });

    sender
}

async fn enqueue_waiting(
    connection: &DatabaseConnection,
    sender: &ProcessingQueueSender,
) -> Result<(), sea_orm::DbErr> {
    let waiting_ids = hyperlink::Entity::find()
        .select_only()
        .column(hyperlink::Column::Id)
        .filter(hyperlink::Column::ProcessingState.eq(HyperlinkProcessingState::Waiting))
        .into_tuple::<i32>()
        .all(connection)
        .await?;

    for hyperlink_id in waiting_ids {
        if sender.send(hyperlink_id).is_err() {
            tracing::warn!(
                hyperlink_id,
                "processing queue dropped while enqueueing waiting hyperlinks"
            );
            break;
        }
    }

    Ok(())
}

async fn process_one(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
) -> Result<(), sea_orm::DbErr> {
    let Some(model) = hyperlink::Entity::find()
        .filter(hyperlink::Column::Id.eq(hyperlink_id))
        .filter(hyperlink::Column::ProcessingState.eq(HyperlinkProcessingState::Waiting))
        .one(connection)
        .await?
    else {
        return Ok(());
    };

    let mut processing_model: hyperlink::ActiveModel = model.into();
    processing_model.processing_state = Set(HyperlinkProcessingState::Processing);
    processing_model.processing_started_at = Set(Some(now_utc()));
    processing_model.processed_at = Set(None);
    processing_model.updated_at = Set(now_utc());
    let processing_model = processing_model.update(connection).await?;

    let mut processing_active_model: hyperlink::ActiveModel = processing_model.into();
    let mut pipeline = Pipeline::new(&mut processing_active_model);
    match pipeline.process(connection).await {
        Ok(()) => {
            processing_active_model.processing_state = Set(HyperlinkProcessingState::Processed);
            processing_active_model.processing_started_at = Set(None);
            processing_active_model.processed_at = Set(Some(now_utc()));
            processing_active_model.updated_at = Set(now_utc());
            processing_active_model.update(connection).await?;
        }
        Err(error) => {
            let message = error.to_string();
            processing_active_model.processing_state = Set(HyperlinkProcessingState::Error);
            processing_active_model.processing_started_at = Set(None);
            processing_active_model.processed_at = Set(None);
            processing_active_model.updated_at = Set(now_utc());
            processing_active_model.update(connection).await?;
            hyperlink_processing_error::insert_new_attempt(connection, hyperlink_id, &message)
                .await?;
            tracing::warn!(hyperlink_id, error = %message, "hyperlink processing failed");
        }
    }

    Ok(())
}

fn now_utc() -> DateTime {
    DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}
