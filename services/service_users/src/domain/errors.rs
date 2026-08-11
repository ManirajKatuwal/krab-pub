#[derive(Debug)]
#[allow(dead_code)]
pub enum DomainError {
    TenantRequired,
    NotFound,
    Unauthorized,
    Internal(String),
}
