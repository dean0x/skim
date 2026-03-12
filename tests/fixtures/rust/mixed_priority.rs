use std::collections::HashMap;
use std::fmt;

pub type UserId = String;
pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug)]
pub enum AppError {
    NotFound(String),
    Validation(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::NotFound(msg) => write!(f, "Not found: {}", msg),
            AppError::Validation(msg) => write!(f, "Validation error: {}", msg),
        }
    }
}

pub trait UserRepository {
    fn find_by_id(&self, id: &UserId) -> Result<User>;
    fn save(&mut self, user: &User) -> Result<()>;
}

pub struct User {
    pub id: UserId,
    pub name: String,
    pub email: String,
}

pub struct InMemoryRepo {
    users: HashMap<UserId, User>,
}

impl InMemoryRepo {
    pub fn new() -> Self {
        Self {
            users: HashMap::new(),
        }
    }
}

impl UserRepository for InMemoryRepo {
    fn find_by_id(&self, id: &UserId) -> Result<User> {
        self.users
            .get(id)
            .map(|u| User {
                id: u.id.clone(),
                name: u.name.clone(),
                email: u.email.clone(),
            })
            .ok_or_else(|| AppError::NotFound(id.clone()))
    }

    fn save(&mut self, user: &User) -> Result<()> {
        self.users.insert(
            user.id.clone(),
            User {
                id: user.id.clone(),
                name: user.name.clone(),
                email: user.email.clone(),
            },
        );
        Ok(())
    }
}

pub fn validate_email(email: &str) -> Result<()> {
    if email.contains('@') {
        Ok(())
    } else {
        Err(AppError::Validation("Invalid email".into()))
    }
}
