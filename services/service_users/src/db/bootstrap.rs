use anyhow::Result;
use krab_core::db::DbPool;
use krab_core::repository::UserRepository;
use sqlx::SqlitePool;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DbDriver {
    Postgres,
    Sqlite,
}

impl DbDriver {
    pub(crate) fn parse(input: &str) -> Result<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "postgres" => Ok(Self::Postgres),
            "sqlite" => Ok(Self::Sqlite),
            other => anyhow::bail!(
                "unsupported KRAB_DB_DRIVER='{}'; supported values are postgres|sqlite",
                other
            ),
        }
    }
}

pub(crate) fn resolve_db_driver() -> Result<DbDriver> {
    let raw = std::env::var("KRAB_DB_DRIVER").unwrap_or_else(|_| "sqlite".to_string());
    DbDriver::parse(&raw)
}

pub(crate) fn default_db_url_for_driver(driver: DbDriver) -> &'static str {
    match driver {
        DbDriver::Postgres => "postgres://postgres@localhost:5432/krab_users",
        DbDriver::Sqlite => "sqlite://krab_users.sqlite?mode=rwc",
    }
}

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
