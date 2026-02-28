use migration::{Migrator, MigratorTrait};
use sea_orm::DatabaseConnection;

pub async fn migrate_pending(connection: &DatabaseConnection) -> Result<(), String> {
    Migrator::up(connection, None)
        .await
        .map_err(|err| format!("failed to run pending migrations: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::Database;

    #[tokio::test]
    async fn migrate_pending_is_idempotent() {
        let connection = Database::connect(crate::db::MEMORY)
            .await
            .expect("in-memory database should connect");

        migrate_pending(&connection)
            .await
            .expect("first migration run should succeed");
        migrate_pending(&connection)
            .await
            .expect("second migration run should succeed");
    }
}
