//! Adds `pipeline_runs.user_id` — the user who triggered the run — plus its
//! index.
//!
//! Python added the same column in alembic revision `a7f3c9e1b5d2`
//! (`cognee/alembic/versions/a7f3c9e1b5d2_extend_pipeline_runs_operation_record.py:36, 54@37f788bd2`):
//! nullable, no backfill, index `ix_pipeline_runs_user_id`, both added only
//! when missing. Rust and Python share one SQLite file and either SDK may
//! migrate it first, so this migration guards the column and the index the
//! same way rather than issuing a plain `ALTER TABLE`.
//!
//! Nullable with no default and no backfill: a row written before this
//! migration has no recorded triggerer, and `user_id IN (…)` never matches
//! `NULL`, so such a row is visible to a user-scoped listing only through its
//! dataset (Python's activity feed behaves the same).
//!
//! `text`, like every id column in this schema: it holds `uuid_hex` (32 hex
//! chars, no hyphens), which is what Python's SQLAlchemy `UUID` renders as on
//! SQLite.
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const INDEX: &str = "ix_pipeline_runs_user_id";

/// Does `pipeline_runs` already have `user_id`?
///
/// `SchemaManager::has_column` is unusable here for the reason given in
/// `m20260916_000001_provenance_pipeline_run_id`: it dispatches on
/// sea-orm-migration's own `sqlx-*` features, which this crate does not enable.
async fn has_user_id(manager: &SchemaManager<'_>) -> Result<bool, DbErr> {
    let backend = manager.get_database_backend();
    let sql = match backend {
        DbBackend::Sqlite => {
            "SELECT COUNT(*) AS cnt FROM pragma_table_info('pipeline_runs') WHERE name = 'user_id'"
        }
        DbBackend::Postgres => {
            "SELECT COUNT(*) AS cnt FROM information_schema.columns \
             WHERE table_schema = current_schema() AND table_name = 'pipeline_runs' \
             AND column_name = 'user_id'"
        }
        other => {
            return Err(DbErr::Custom(format!(
                "pipeline_runs.user_id migration does not support the {other:?} backend"
            )));
        }
    };
    let row = manager
        .get_connection()
        .query_one(Statement::from_string(backend, sql))
        .await?
        .ok_or_else(|| DbErr::Custom("column-existence probe returned no row".to_owned()))?;
    Ok(row.try_get::<i64>("", "cnt")? > 0)
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !has_user_id(manager).await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineRuns::Table)
                        .add_column(ColumnDef::new(PipelineRuns::UserId).text().null())
                        .to_owned(),
                )
                .await?;
        }
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(INDEX)
                    .table(PipelineRuns::Table)
                    .col(PipelineRuns::UserId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Index first, then column — the reverse of `up`.
        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name(INDEX)
                    .table(PipelineRuns::Table)
                    .to_owned(),
            )
            .await?;
        if has_user_id(manager).await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineRuns::Table)
                        .drop_column(PipelineRuns::UserId)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum PipelineRuns {
    Table,
    UserId,
}
