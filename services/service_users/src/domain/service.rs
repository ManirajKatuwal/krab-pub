use std::sync::Arc;

use async_trait::async_trait;
use krab_core::repository::UserRepository;

use super::errors::DomainError;
use super::models::UserModel;

#[async_trait]
pub trait UserDomainService: Send + Sync {
    async fn get_me(&self, tenant_id: &str) -> Result<UserModel, DomainError>;
    #[allow(dead_code)]
    async fn get_user_by_id(&self, id: &str, tenant_id: &str) -> Result<UserModel, DomainError>;
}

pub struct UserDomainServiceImpl {
    repo: Arc<dyn UserRepository>,
}

impl UserDomainServiceImpl {
    pub fn new(repo: Arc<dyn UserRepository>) -> Self {
        Self { repo }
    }
}

#[async_trait]
impl UserDomainService for UserDomainServiceImpl {
    async fn get_me(&self, tenant_id: &str) -> Result<UserModel, DomainError> {
        let tenant = tenant_id.trim();
        if tenant.is_empty() {
            return Err(DomainError::TenantRequired);
        }

        let record = self
            .repo
            .find_first_by_tenant(tenant)
            .await
            .map_err(|e| DomainError::Internal(e.to_string()))?;

        match record {
            Some(r) => Ok(UserModel {
                id: r.id,
                username: r.username,
            }),
            None => Ok(UserModel {
                id: "1".to_string(),
                username: "krab_user".to_string(),
            }),
        }
    }

    async fn get_user_by_id(&self, _id: &str, _tenant_id: &str) -> Result<UserModel, DomainError> {
        Err(DomainError::NotFound)
    }
}
