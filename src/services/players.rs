//! Player service — sqlx-backed data access for the `players` table.
//!
//! This module concentrates all SQL touching the `players` table so that the
//! HTTP layer (`src/api/auth.rs`, `src/api/players.rs`) deals only in domain
//! types. Errors are projected through [`PlayerServiceError`] which the API
//! layer maps to HTTP status codes.

use crate::entities::player::Player;
use crate::services::auth as auth_svc;
use sqlx::MySqlPool;
use thiserror::Error;
use uuid::Uuid;

/// Domain-level errors for player operations.
#[derive(Debug, Error)]
pub enum PlayerServiceError {
    #[error("a player with that username or email already exists")]
    Duplicate,
    #[error("player not found")]
    NotFound,
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("database error")]
    Database(#[source] sqlx::Error),
    #[error("internal error")]
    Internal(#[source] anyhow::Error),
}

/// Input for [`create_player`]. Mirrors `POST /auth/register`.
#[derive(Debug, Clone)]
pub struct NewPlayer<'a> {
    pub username: &'a str,
    pub email: Option<&'a str>,
    pub password_plain: &'a str,
    pub parent_account_public_id: Option<&'a str>,
}

/// Insert a new player, hashing the password with Argon2id first.
///
/// Returns the freshly persisted [`Player`]. Maps unique-violation errors
/// to [`PlayerServiceError::Duplicate`] so the API layer can return 409.
pub async fn create_player(
    pool: &MySqlPool,
    input: NewPlayer<'_>,
) -> Result<Player, PlayerServiceError> {
    let username = input.username.trim();
    if username.is_empty() || username.len() > 64 {
        return Err(PlayerServiceError::Invalid(
            "username must be 1..=64 characters".into(),
        ));
    }
    if let Some(e) = input.email
        && e.len() > 255
    {
        return Err(PlayerServiceError::Invalid(
            "email exceeds 255 chars".into(),
        ));
    }

    let password_hash =
        auth_svc::hash_password(input.password_plain).map_err(PlayerServiceError::Internal)?;

    // Resolve parent public_id -> internal id, if supplied.
    let parent_account_id: Option<u64> = if let Some(pp) = input.parent_account_public_id {
        let row: Option<(u64,)> = sqlx::query_as("SELECT id FROM players WHERE public_id = ?")
            .bind(pp)
            .fetch_optional(pool)
            .await
            .map_err(PlayerServiceError::Database)?;
        match row {
            Some((id,)) => Some(id),
            None => {
                return Err(PlayerServiceError::Invalid(
                    "parent_account_public_id does not refer to an existing player".into(),
                ));
            }
        }
    } else {
        None
    };

    let public_id = Uuid::new_v4().to_string();

    let res = sqlx::query(
        r#"
        INSERT INTO players (public_id, username, email, password_hash, parent_account_id)
        VALUES (?, ?, ?, ?, ?)
        "#,
    )
    .bind(&public_id)
    .bind(username)
    .bind(input.email)
    .bind(&password_hash)
    .bind(parent_account_id)
    .execute(pool)
    .await;

    match res {
        Ok(_) => find_by_public_id(pool, &public_id)
            .await?
            .ok_or(PlayerServiceError::NotFound),
        Err(sqlx::Error::Database(db_err)) if is_unique_violation(db_err.as_ref()) => {
            Err(PlayerServiceError::Duplicate)
        }
        Err(e) => Err(PlayerServiceError::Database(e)),
    }
}

/// Look up a player by `public_id` (UUID).
pub async fn find_by_public_id(
    pool: &MySqlPool,
    public_id: &str,
) -> Result<Option<Player>, PlayerServiceError> {
    sqlx::query_as::<_, Player>(
        r#"
        SELECT id, public_id, username, email, password_hash,
               parent_account_id, status, created_at, updated_at, deleted_at
        FROM players
        WHERE public_id = ? AND status <> 'deleted'
        "#,
    )
    .bind(public_id)
    .fetch_optional(pool)
    .await
    .map_err(PlayerServiceError::Database)
}

/// Find a player by their login identifier (username today; email is a
/// future extension once email-login flows are designed).
pub async fn find_by_username(
    pool: &MySqlPool,
    username: &str,
) -> Result<Option<Player>, PlayerServiceError> {
    sqlx::query_as::<_, Player>(
        r#"
        SELECT id, public_id, username, email, password_hash,
               parent_account_id, status, created_at, updated_at, deleted_at
        FROM players
        WHERE username = ? AND status <> 'deleted'
        "#,
    )
    .bind(username)
    .fetch_optional(pool)
    .await
    .map_err(PlayerServiceError::Database)
}

/// Verify a username/password pair. On success returns the player.
///
/// Returns `Ok(None)` for "wrong credentials" so the caller can return a
/// uniform 401 without leaking which half of the pair was wrong.
pub async fn verify_credentials(
    pool: &MySqlPool,
    username: &str,
    password_plain: &str,
) -> Result<Option<Player>, PlayerServiceError> {
    let Some(p) = find_by_username(pool, username).await? else {
        return Ok(None);
    };
    let ok = auth_svc::verify_password(password_plain, &p.password_hash)
        .map_err(PlayerServiceError::Internal)?;
    if ok { Ok(Some(p)) } else { Ok(None) }
}

/// MySQL `ER_DUP_ENTRY` is error code 1062.
fn is_unique_violation(err: &dyn sqlx::error::DatabaseError) -> bool {
    err.code().as_deref() == Some("23000")
        || err
            .message()
            .to_ascii_lowercase()
            .contains("duplicate entry")
}
