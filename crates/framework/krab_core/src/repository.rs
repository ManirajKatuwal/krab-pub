use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct UserRecord {
    pub id: String,
    pub username: String,
}

#[async_trait]
pub trait UserRepository: Send + Sync {
    async fn find_first_by_tenant(&self, tenant_id: &str) -> Result<Option<UserRecord>>;
}

/// Default in-memory repository for local/dev and tests.
#[derive(Clone, Default)]
pub struct InMemoryUserRepository {
    users_by_tenant: Arc<RwLock<HashMap<String, UserRecord>>>,
}

impl InMemoryUserRepository {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn insert_for_tenant(&self, tenant_id: impl Into<String>, user: UserRecord) {
        self.users_by_tenant
            .write()
            .await
            .insert(tenant_id.into(), user);
    }

    pub async fn remove_tenant(&self, tenant_id: &str) -> Option<UserRecord> {
        self.users_by_tenant.write().await.remove(tenant_id)
    }
}

#[async_trait]
impl UserRepository for InMemoryUserRepository {
    async fn find_first_by_tenant(&self, tenant_id: &str) -> Result<Option<UserRecord>> {
        Ok(self.users_by_tenant.read().await.get(tenant_id).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_repo_returns_inserted_user() {
        let repo = InMemoryUserRepository::new();
        repo.insert_for_tenant(
            "tenant-a",
            UserRecord {
                id: "u1".to_string(),
                username: "alice".to_string(),
            },
        )
        .await;

        let user = repo
            .find_first_by_tenant("tenant-a")
            .await
            .expect("repository lookup should succeed");

        assert!(user.is_some());
        let user = user.expect("user should be present");
        assert_eq!(user.id, "u1");
        assert_eq!(user.username, "alice");
    }

    #[tokio::test]
    async fn in_memory_repo_returns_none_for_missing_tenant() {
        let repo = InMemoryUserRepository::new();
        let user = repo
            .find_first_by_tenant("missing")
            .await
            .expect("repository lookup should succeed");
        assert!(user.is_none());
    }
}
