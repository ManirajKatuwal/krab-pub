#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserModel {
    pub id: String,
    pub username: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum DomainError {
    TenantRequired,
    NotFound,
    Unauthorized,
    Internal(String),
}
