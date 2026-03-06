use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Hyperlink::Table)
                    .add_column(
                        ColumnDef::new(Hyperlink::SourceType)
                            .string_len(16)
                            .not_null()
                            .default("unknown"),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                r#"
                UPDATE hyperlink
                SET source_type = COALESCE(
                    (
                        SELECT CASE a.kind
                            WHEN 'pdf_source' THEN 'pdf'
                            WHEN 'snapshot_warc' THEN 'html'
                            ELSE 'unknown'
                        END
                        FROM hyperlink_artifact a
                        WHERE a.hyperlink_id = hyperlink.id
                          AND a.kind IN ('pdf_source', 'snapshot_warc')
                        ORDER BY a.created_at DESC, a.id DESC
                        LIMIT 1
                    ),
                    'unknown'
                );
                "#,
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Hyperlink::Table)
                    .drop_column(Hyperlink::SourceType)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Hyperlink {
    Table,
    SourceType,
}
