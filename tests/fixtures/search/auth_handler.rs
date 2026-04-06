use std::collections::HashMap;

/// Handler for authentication and token validation.
pub struct AuthHandler {
    secret_key: String,
    token_cache: HashMap<String, TokenInfo>,
}

/// Information about a validated token.
pub struct TokenInfo {
    pub user_id: String,
    pub expires_at: u64,
    pub scopes: Vec<String>,
}

/// Errors that can occur during authentication.
pub enum AuthError {
    InvalidToken,
    ExpiredToken,
    InsufficientScope(String),
}

impl AuthHandler {
    pub fn new(secret_key: String) -> Self {
        Self {
            secret_key,
            token_cache: HashMap::new(),
        }
    }

    /// Validate a bearer token and return its claims.
    pub fn validate_token(&self, token: &str) -> Result<&TokenInfo, AuthError> {
        self.token_cache
            .get(token)
            .ok_or(AuthError::InvalidToken)
    }

    /// Check if a token has the required scope.
    pub fn check_scope(&self, token: &str, required: &str) -> Result<(), AuthError> {
        let info = self.validate_token(token)?;
        if info.scopes.iter().any(|s| s == required) {
            Ok(())
        } else {
            Err(AuthError::InsufficientScope(required.to_string()))
        }
    }
}
