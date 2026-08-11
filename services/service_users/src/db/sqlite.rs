use anyhow::{Context as _, Result};
use async_trait::async_trait;
use krab_core::repository::{UserRecord, UserRepository};
use sqlx::sqlite::SqlitePool;
use sqlx::Row;

use crate::SqlDialect;

#[derive(Clone)]
pub struct SqliteUserRepository {
    pool: SqlitePool,
}

impl SqliteUserRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[derive(Debug, Clone)]
struct UserQueryModel {
    id: String,
    username: String,
}

impl<'r, R> sqlx::FromRow<'r, R> for UserQueryModel
where
    R: Row,
    &'r str: sqlx::ColumnIndex<R>,
    String: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
{
    fn from_row(row: &'r R) -> std::result::Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            username: row.try_get("username")?,
        })
    }
}

#[async_trait]
impl UserRepository for SqliteUserRepository {
    async fn find_first_by_tenant(&self, tenant_id: &str) -> Result<Option<UserRecord>> {
        let sql = SqlDialect::Sqlite.users_me_tenant_scoped_sql();
        let model = sqlx::query_as::<_, UserQueryModel>(&sql)
            .bind(tenant_id)
            .fetch_optional(&self.pool)
            .await
            .context("failed to query tenant-scoped user (sqlite)")?;

        Ok(model.map(|model| UserRecord {
            id: model.id,
            username: model.username,
        }))
    }
}
