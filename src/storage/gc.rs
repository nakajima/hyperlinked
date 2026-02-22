use std::time::Duration;

use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, Value};
use tokio::time::sleep;

use super::artifacts;

const DEFAULT_INTERVAL_SECS: u64 = 15;
const DEFAULT_BATCH_SIZE: u64 = 64;

pub fn spawn(connection: DatabaseConnection) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let interval = Duration::from_secs(env_u64(
            "ARTIFACT_GC_INTERVAL_SECS",
            DEFAULT_INTERVAL_SECS,
            1,
            3600,
        ));
        let batch_size = env_u64("ARTIFACT_GC_BATCH_SIZE", DEFAULT_BATCH_SIZE, 1, 10_000);

        loop {
            if let Err(err) = run_once(&connection, batch_size).await {
                tracing::warn!(error = %err, "artifact gc iteration failed");
            }
            sleep(interval).await;
        }
    })
}

async fn run_once(connection: &DatabaseConnection, batch_size: u64) -> Result<(), String> {
    let backend = connection.get_database_backend();
    let rows = connection
        .query_all(Statement::from_sql_and_values(
            backend,
            r#"
            SELECT id, storage_path, attempts
            FROM artifact_gc_pending
            WHERE next_attempt_at <= CURRENT_TIMESTAMP
            ORDER BY id ASC
            LIMIT ?
            "#
            .to_string(),
            vec![Value::from(batch_size as i64)],
        ))
        .await;
    let rows = match rows {
        Ok(rows) => rows,
        Err(err)
            if err
                .to_string()
                .contains("no such table: artifact_gc_pending") =>
        {
            return Ok(());
        }
        Err(err) => return Err(format!("failed to load artifact gc queue: {err}")),
    };

    for row in rows {
        let id: i64 = row
            .try_get("", "id")
            .map_err(|err| format!("failed to decode artifact gc id: {err}"))?;
        let storage_path: String = row
            .try_get("", "storage_path")
            .map_err(|err| format!("failed to decode artifact gc storage_path: {err}"))?;
        let attempts: i64 = row
            .try_get("", "attempts")
            .map_err(|err| format!("failed to decode artifact gc attempts: {err}"))?;

        match artifacts::delete_if_exists(&storage_path).await {
            Ok(_) => {
                connection
                    .execute(Statement::from_sql_and_values(
                        backend,
                        "DELETE FROM artifact_gc_pending WHERE id = ?".to_string(),
                        vec![Value::from(id)],
                    ))
                    .await
                    .map_err(|err| format!("failed to delete gc queue row {id}: {err}"))?;
            }
            Err(error) => {
                let next_attempts = attempts.saturating_add(1);
                let backoff_secs = retry_backoff_secs(next_attempts as u32);
                let modifier = format!("+{backoff_secs} seconds");

                connection
                    .execute(Statement::from_sql_and_values(
                        backend,
                        r#"
                        UPDATE artifact_gc_pending
                        SET attempts = ?,
                            last_error = ?,
                            next_attempt_at = DATETIME(CURRENT_TIMESTAMP, ?),
                            updated_at = CURRENT_TIMESTAMP
                        WHERE id = ?
                        "#
                        .to_string(),
                        vec![
                            Value::from(next_attempts),
                            Value::from(error),
                            Value::from(modifier),
                            Value::from(id),
                        ],
                    ))
                    .await
                    .map_err(|err| format!("failed to update gc queue row {id}: {err}"))?;
            }
        }
    }

    Ok(())
}

fn retry_backoff_secs(attempts: u32) -> u64 {
    let exponent = attempts.saturating_sub(1).min(6);
    2u64.saturating_pow(exponent).saturating_mul(15)
}

fn env_u64(key: &str, default: u64, min: u64, max: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| value.clamp(min, max))
        .unwrap_or(default.clamp(min, max))
}
