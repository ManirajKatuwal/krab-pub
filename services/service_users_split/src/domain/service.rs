use std::sync::Arc;

use async_trait::async_trait;

use super::models::{DomainError, UserModel};

#[async_trait]
pub trait DomainService: Send + Sync {
    async fn get_me(&self, tenant_id: &str) -> Result<UserModel, DomainError>;
}

pub struct InMemoryDomainService;

impl InMemoryDomainService {
    pub fn new() -> Self {
        Self
    }

    pub fn shared() -> Arc<dyn DomainService> {
        Arc::new(Self::new())
    }
}

impl Default for InMemoryDomainService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DomainService for InMemoryDomainService {
    async fn get_me(&self, tenant_id: &str) -> Result<UserModel, DomainError> {
        let tenant = tenant_id.trim();
        if tenant.is_empty() {
            return Err(DomainError::TenantRequired);
        }

        Ok(UserModel {
            id: "1".to_string(),
            username: format!("krab_user_{tenant}"),
        })
    }
}
