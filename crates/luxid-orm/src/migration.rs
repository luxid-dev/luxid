//! Migrations.
//!
//! Luxid does not reimplement SeaORM's migrator; it wraps it, so migrations are
//! ordinary `MigrationTrait` implementations and the cross-dialect DDL builder
//! comes along unchanged. What Luxid adds is Luxid's error type and a status
//! you can render, rather than one that prints to stdout.

use luxid_core::error::{Error, Result};
use sea_orm_migration::MigratorTrait;

use crate::db::Db;

/// Everything needed to write a migration.
///
/// One gotcha worth knowing: SeaORM's `DeriveMigrationName` takes the migration
/// name from the **file stem**, not the struct name — so two migrations in one
/// file share a name and the second is treated as already applied. One
/// migration per file, named `m20260101_000001_create_posts.rs`. Implement
/// `MigrationName` by hand if you need a name the filename cannot give.
pub mod prelude {
    pub use sea_orm_migration::prelude::*;
    /// `pk_auto`, `string`, `integer`, `timestamp`, … — column shorthands that
    /// SeaORM keeps out of its own prelude.
    pub use sea_orm_migration::schema::*;
}

/// Whether a migration has run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationState {
    pub name: String,
    pub applied: bool,
}

fn failed(action: &str, err: sea_orm::DbErr) -> Error {
    Error::internal(format!("{action} failed: {err}"))
}

impl Db {
    /// Apply every pending migration.
    pub async fn migrate<M: MigratorTrait>(&self) -> Result<()> {
        M::up(self.connection(), None)
            .await
            .map_err(|err| failed("migrate", err))
    }

    /// Apply at most `steps` pending migrations.
    pub async fn migrate_steps<M: MigratorTrait>(&self, steps: u32) -> Result<()> {
        M::up(self.connection(), Some(steps))
            .await
            .map_err(|err| failed("migrate", err))
    }

    /// Roll back the last `steps` applied migrations.
    pub async fn rollback<M: MigratorTrait>(&self, steps: u32) -> Result<()> {
        M::down(self.connection(), Some(steps))
            .await
            .map_err(|err| failed("rollback", err))
    }

    /// Drop everything and migrate from scratch. Never run this against data
    /// you want to keep.
    pub async fn migrate_fresh<M: MigratorTrait>(&self) -> Result<()> {
        M::fresh(self.connection())
            .await
            .map_err(|err| failed("migrate:fresh", err))
    }

    /// Every migration and whether it has been applied, in declaration order.
    pub async fn migration_status<M: MigratorTrait>(&self) -> Result<Vec<MigrationState>> {
        let applied = M::get_applied_migrations(self.connection())
            .await
            .map_err(|err| failed("migrate:status", err))?;

        let pending = M::get_pending_migrations(self.connection())
            .await
            .map_err(|err| failed("migrate:status", err))?;

        let mut states: Vec<MigrationState> = applied
            .into_iter()
            .map(|migration| MigrationState {
                name: migration.name().to_owned(),
                applied: true,
            })
            .collect();

        states.extend(pending.into_iter().map(|migration| MigrationState {
            name: migration.name().to_owned(),
            applied: false,
        }));

        Ok(states)
    }

    /// Whether anything is waiting to run — worth checking at boot.
    pub async fn has_pending_migrations<M: MigratorTrait>(&self) -> Result<bool> {
        M::get_pending_migrations(self.connection())
            .await
            .map(|pending| !pending.is_empty())
            .map_err(|err| failed("migrate:status", err))
    }
}
