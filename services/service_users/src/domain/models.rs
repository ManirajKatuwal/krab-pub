#[derive(Debug, Clone)]
pub struct UserModel {
    pub id: String,
    pub username: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct UserProfile {
    pub user_id: String,
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub avatar_url: Option<String>,
}
