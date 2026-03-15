use super::*;
use sea_orm::Database;

#[tokio::test]
async fn ensure_current_is_idempotent() {
    let connection = Database::connect(crate::db::MEMORY)
        .await
        .expect("in-memory database should connect");

    sync(&connection)
        .await
        .expect("first schema sync should succeed");
    sync(&connection)
        .await
        .expect("second schema sync should succeed");
}
