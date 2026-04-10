/// User authentication module for the API server.
///
/// Provides JWT-based authentication with bcrypt password hashing.
use serde::{Deserialize, Serialize};
use anyhow::Result;

/// Request payload for user login.
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

/// Response payload for a successful login.
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub expires_in: u64,
}

/// Internal token claims for JWT generation.
struct TokenClaims {
    sub: String,
    exp: u64,
    iat: u64,
}

impl TokenClaims {
    fn new(subject: &str, ttl_seconds: u64) -> Self {
        let now = current_unix_timestamp();
        Self {
            sub: subject.to_string(),
            exp: now + ttl_seconds,
            iat: now,
        }
    }
}

/// Handle user login.
///
/// Validates credentials against the database and returns a signed JWT
/// token if the credentials are correct.
pub async fn handle_login(req: LoginRequest, db: &Database) -> Result<LoginResponse> {
    let user = db.find_user_by_username(&req.username).await?;
    verify_password(&req.password, &user.password_hash)?;
    let claims = TokenClaims::new(&req.username, 3600);
    let token = sign_jwt(&claims)?;
    Ok(LoginResponse {
        token,
        expires_in: 3600,
    })
}

/// Verify a plaintext password against a bcrypt hash.
fn verify_password(plaintext: &str, hash: &str) -> Result<()> {
    if bcrypt::verify(plaintext, hash)? {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Invalid credentials"))
    }
}

fn sign_jwt(claims: &TokenClaims) -> Result<String> {
    // Implementation uses jsonwebtoken crate
    todo!("JWT signing implementation")
}

fn current_unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
