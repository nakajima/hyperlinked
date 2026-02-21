use crate::{
    entity::hyperlink,
    model::hyperlink_processing_job::ProcessingQueueSender,
    processors::{
        processor::{ProcessingError, Processor},
        readability_fetch::{ReadabilityFetchOutput, ReadabilityFetcher},
        snapshot_fetch::{SnapshotFetchOutput, SnapshotFetcher},
        sublink_discovery::{SublinkDiscoveryOutput, SublinkDiscoveryProcessor},
        title_fetch::TitleFetcher,
    },
};
use sea_orm::DatabaseConnection;

pub struct Pipeline<'a> {
    pub hyperlink: &'a mut hyperlink::ActiveModel,
    pub job_id: i32,
    pub processing_queue: Option<ProcessingQueueSender>,
}

impl<'a> Pipeline<'a> {
    pub fn new(
        hyperlink: &'a mut hyperlink::ActiveModel,
        job_id: i32,
        processing_queue: Option<ProcessingQueueSender>,
    ) -> Self {
        Self {
            hyperlink,
            job_id,
            processing_queue,
        }
    }

    pub async fn process_snapshot(
        &mut self,
        connection: &DatabaseConnection,
    ) -> Result<SnapshotFetchOutput, ProcessingError> {
        TitleFetcher {}.process(self.hyperlink, connection).await?;
        SnapshotFetcher::new(self.job_id)
            .process(self.hyperlink, connection)
            .await
    }

    pub async fn process_readability(
        &mut self,
        connection: &DatabaseConnection,
    ) -> Result<ReadabilityFetchOutput, ProcessingError> {
        ReadabilityFetcher::new(self.job_id)
            .process(self.hyperlink, connection)
            .await
    }

    pub async fn process_sublink_discovery(
        &mut self,
        connection: &DatabaseConnection,
    ) -> Result<SublinkDiscoveryOutput, ProcessingError> {
        SublinkDiscoveryProcessor::new(self.processing_queue.clone())
            .process(self.hyperlink, connection)
            .await
    }
}
