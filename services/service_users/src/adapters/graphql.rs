use std::sync::Arc;

use async_graphql::{Context, EmptyMutation, EmptySubscription, Object, Schema, SimpleObject};
use krab_core::http::AuthContext;

use crate::domain::errors::DomainError;
use crate::domain::service::UserDomainService;

#[derive(SimpleObject)]
pub struct User {
    pub id: String,
    pub username: String,
}

pub struct UserQuery;

#[Object]
impl UserQuery {
    async fn me(&self, ctx: &Context<'_>) -> async_graphql::Result<User> {
        let domain = ctx.data::<Arc<dyn UserDomainService>>()?;
        let auth = ctx.data::<AuthContext>()?;

        let tenant_id = auth
            .tenant_id
            .as_deref()
            .ok_or_else(|| async_graphql::Error::new("tenant context is required"))?;

        let model = domain
            .get_me(tenant_id)
            .await
            .map_err(domain_error_to_graphql)?;

        Ok(User {
            id: model.id,
            username: model.username,
        })
    }
}

pub type UsersSchema = Schema<UserQuery, EmptyMutation, EmptySubscription>;

pub fn build_schema(domain: Arc<dyn UserDomainService>) -> UsersSchema {
    Schema::build(UserQuery, EmptyMutation, EmptySubscription)
        .data(domain)
        .finish()
}

fn domain_error_to_graphql(err: DomainError) -> async_graphql::Error {
    match err {
        DomainError::TenantRequired => async_graphql::Error::new("tenant context is required"),
        DomainError::NotFound => async_graphql::Error::new("user not found"),
        DomainError::Unauthorized => async_graphql::Error::new("access denied"),
        DomainError::Internal(msg) => async_graphql::Error::new(msg),
    }
}
