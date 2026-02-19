use sea_orm::DatabaseConnection;

#[derive(Clone)]
pub struct Context {
    pub connection: DatabaseConnection,
}
