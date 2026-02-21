use sea_orm::{DatabaseConnection, DbErr};

use crate::entity::hyperlink;

#[derive(Debug)]
pub enum ProcessingError {
    FetchError(String),
    DB(DbErr),
}

impl std::fmt::Display for ProcessingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FetchError(message) => write!(f, "{message}"),
            Self::DB(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for ProcessingError {}

pub trait Processor {
    type Output;

    fn process<'a>(
        &'a mut self,
        hyperlink: &'a mut hyperlink::ActiveModel,
        connection: &'a DatabaseConnection,
    ) -> impl std::future::Future<Output = Result<Self::Output, ProcessingError>> + Send + 'a;
}
