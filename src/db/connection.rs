use sea_orm::{Database, DatabaseConnection, DbErr};

const MEMORY: &'static str = "sqlite::memory:";

pub async fn init() -> Result<DatabaseConnection, DbErr> {
    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| MEMORY.to_string());
    Database::connect(database_url).await
}
