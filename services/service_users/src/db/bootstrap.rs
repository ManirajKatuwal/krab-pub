use anyhow::Result;
use krab_core::db::DbPool;
use krab_core::repository::UserRepository;
use sqlx::SqlitePool;
use std::sync::Arc;

// Driver selection is framework surface, not application code. `DbDriver`,
// `resolve_db_driver`, and `default_db_url_for_driver` were defined here — in a
// reference service — so `KRAB_DB_DRIVER` was advertised as a framework-level
// choice that no framework consumer could actually make. They now live in
// `krab_core::db`; these re-exports keep the call sites in this crate unchanged.
//
// What stays here is genuinely application-specific: the pool wrapper and the
// repository implementations, which depend on this service's schema.
pub(crate) use krab_core::db::{default_db_url_for_driver, resolve_db_driver, DbDriver};

#[derive(Clone)]
pub(crate) enum UsersDbPool {
    Postgres(DbPool),
    Sqlite(SqlitePool),
}

impl UsersDbPool {
    pub(crate) fn dependency_name(&self) -> &'static str {
        match self {
            Self::Postgres(_) => "postgres",
            Self::Sqlite(_) => "sqlite",
        }
    }

    pub(crate) fn try_acquire_available(&self) -> bool {
        match self {
            Self::Postgres(pool) => pool.try_acquire().is_some(),
            Self::Sqlite(pool) => pool.try_acquire().is_some(),
        }
    }
}

pub(crate) fn build_user_repository(
    driver: DbDriver,
    pool: &UsersDbPool,
) -> Result<Arc<dyn UserRepository>> {
    match (driver, pool) {
        (DbDriver::Postgres, UsersDbPool::Postgres(pool)) => Ok(Arc::new(
            crate::db::postgres::PostgresUserRepository::new(pool.clone()),
        )),
        (DbDriver::Sqlite, UsersDbPool::Sqlite(pool)) => Ok(Arc::new(
            crate::db::sqlite::SqliteUserRepository::new(pool.clone()),
        )),
        (DbDriver::Postgres, UsersDbPool::Sqlite(_)) => {
            anyhow::bail!("database driver/pool mismatch: postgres driver requires postgres pool")
        }
        (DbDriver::Sqlite, UsersDbPool::Postgres(_)) => {
            anyhow::bail!("database driver/pool mismatch: sqlite driver requires sqlite pool")
        }
    }
}
