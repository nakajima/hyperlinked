use crate::{
    entity::hyperlink,
    processors::{
        processor::{ProcessingError, Processor},
        title_fetch::TitleFetcher,
    },
};
use sea_orm::DatabaseConnection;

pub struct Pipeline<'a> {
    pub hyperlink: &'a mut hyperlink::ActiveModel,
}

impl<'a> Pipeline<'a> {
    pub fn new(hyperlink: &'a mut hyperlink::ActiveModel) -> Self {
        Self { hyperlink }
    }

    pub async fn process(
        &mut self,
        connection: &DatabaseConnection,
    ) -> Result<(), ProcessingError> {
        TitleFetcher {}.process(self.hyperlink, connection).await?;
        Ok(())
    }
}
