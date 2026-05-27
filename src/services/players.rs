//! Player service — sqlx-backed data access for the `players` table.
//!
//! This module concentrates all SQL touching the `players` table so that the
//! HTTP layer (`src/api/auth.rs`, `src/api/players.rs`) deals only in domain
//! types. Errors are projected through [`PlayerServiceError`] which the API
//! layer maps to HTTP status codes.

use crate::entities::player::{Player, PlayerView};
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
    #[error("forbidden: you may only modify your own account")]
    Forbidden,
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

/// Input for [`update_player`]. All fields are optional; absent fields keep
/// their current values.
#[derive(Debug, Clone, Default)]
pub struct UpdatePlayerInput {
    /// New username. `None` leaves the current value unchanged.
    pub username: Option<String>,
    /// New email address. `None` leaves the current value unchanged.
    pub email: Option<String>,
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

/// Look up a player and return the public-facing [`PlayerView`], resolving the
/// parent account's `public_id` when a parent exists.
pub async fn find_player_view(
    pool: &MySqlPool,
    public_id: &str,
) -> Result<Option<PlayerView>, PlayerServiceError> {
    let Some(player) = find_by_public_id(pool, public_id).await? else {
        return Ok(None);
    };

    let parent_public_id = if let Some(parent_id) = player.parent_account_id {
        let row: Option<(String,)> = sqlx::query_as("SELECT public_id FROM players WHERE id = ?")
            .bind(parent_id)
            .fetch_optional(pool)
            .await
            .map_err(PlayerServiceError::Database)?;
        row.map(|(pid,)| pid)
    } else {
        None
    };

    let mut view = PlayerView::from(&player);
    view.parent_account_id = parent_public_id;
    Ok(Some(view))
}

/// Find a player by their login identifier (username).
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

/// Update a player's mutable profile fields.
///
/// Only the owning player (where `public_id == requesting_player_id`) may
/// modify the account. Returns [`PlayerServiceError::Forbidden`] for
/// cross-account attempts and [`PlayerServiceError::Duplicate`] when the new
/// username or email is already taken.
pub async fn update_player(
    pool: &MySqlPool,
    public_id: &str,
    requesting_player_id: &str,
    input: UpdatePlayerInput,
) -> Result<Player, PlayerServiceError> {
    if public_id != requesting_player_id {
        return Err(PlayerServiceError::Forbidden);
    }

    let player = find_by_public_id(pool, public_id)
        .await?
        .ok_or(PlayerServiceError::NotFound)?;

    // Apply the updates, defaulting to current values for omitted fields.
    let new_username = input
        .username
        .as_deref()
        .unwrap_or(&player.username)
        .trim()
        .to_owned();
    let new_email: Option<String> = input.email.or_else(|| player.email.clone());

    if new_username.is_empty() || new_username.len() > 64 {
        return Err(PlayerServiceError::Invalid(
            "username must be 1..=64 characters".into(),
        ));
    }
    if let Some(ref e) = new_email
        && e.len() > 255
    {
        return Err(PlayerServiceError::Invalid(
            "email exceeds 255 chars".into(),
        ));
    }

    let res = sqlx::query(
        "UPDATE players SET username = ?, email = ?, updated_at = NOW() \
         WHERE id = ? AND status <> 'deleted'",
    )
    .bind(&new_username)
    .bind(new_email.as_deref())
    .bind(player.id)
    .execute(pool)
    .await;

    match res {
        Ok(r) if r.rows_affected() == 0 => Err(PlayerServiceError::NotFound),
        Ok(_) => find_by_public_id(pool, public_id)
            .await?
            .ok_or(PlayerServiceError::NotFound),
        Err(sqlx::Error::Database(db_err)) if is_unique_violation(db_err.as_ref()) => {
            Err(PlayerServiceError::Duplicate)
        }
        Err(e) => Err(PlayerServiceError::Database(e)),
    }
}

/// Soft-delete a player by setting `status = 'deleted'` and `deleted_at = NOW()`.
///
/// Only the owning player may delete their own account. The row is retained
/// for FK integrity and filtered out by all other service queries.
/// Returns [`PlayerServiceError::NotFound`] when the player does not exist or
/// is already deleted.
pub async fn soft_delete_player(
    pool: &MySqlPool,
    public_id: &str,
    requesting_player_id: &str,
) -> Result<(), PlayerServiceError> {
    if public_id != requesting_player_id {
        return Err(PlayerServiceError::Forbidden);
    }

    let res = sqlx::query(
        "UPDATE players \
         SET status = 'deleted', deleted_at = NOW(), updated_at = NOW() \
         WHERE public_id = ? AND status <> 'deleted'",
    )
    .bind(public_id)
    .execute(pool)
    .await
    .map_err(PlayerServiceError::Database)?;

    if res.rows_affected() == 0 {
        Err(PlayerServiceError::NotFound)
    } else {
        Ok(())
    }
}

/// MySQL `ER_DUP_ENTRY` is error code 1062.
fn is_unique_violation(err: &dyn sqlx::error::DatabaseError) -> bool {
    err.code().as_deref() == Some("23000")
        || err
            .message()
            .to_ascii_lowercase()
            .contains("duplicate entry")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a lazy pool that will never actually connect.
    /// Safe to use for tests that return before issuing any SQL.
    fn fake_pool() -> MySqlPool {
        MySqlPool::connect_lazy("mysql://test:test@127.0.0.1/test")
            .expect("lazy pool creation should not fail")
    }

    #[tokio::test]
    async fn update_player_rejects_cross_account() {
        let pool = fake_pool();
        let result =
            update_player(&pool, "player-A", "player-B", UpdatePlayerInput::default()).await;
        assert!(
            matches!(result, Err(PlayerServiceError::Forbidden)),
            "expected Forbidden, got {result:?}"
        );
    }

    #[tokio::test]
    async fn soft_delete_rejects_cross_account() {
        let pool = fake_pool();
        let result = soft_delete_player(&pool, "player-A", "player-B").await;
        assert!(
            matches!(result, Err(PlayerServiceError::Forbidden)),
            "expected Forbidden, got {result:?}"
        );
    }

    #[tokio::test]
    async fn update_player_same_account_proceeds_to_db() {
        // Same requesting_player_id — ownership check passes, then we hit the
        // (non-existent) fake DB and get a Database error, not Forbidden.
        let pool = fake_pool();
        let result =
            update_player(&pool, "player-A", "player-A", UpdatePlayerInput::default()).await;
        assert!(
            !matches!(result, Err(PlayerServiceError::Forbidden)),
            "should not return Forbidden for own account"
        );
    }

    #[tokio::test]
    async fn soft_delete_same_account_proceeds_to_db() {
        let pool = fake_pool();
        let result = soft_delete_player(&pool, "player-A", "player-A").await;
        assert!(
            !matches!(result, Err(PlayerServiceError::Forbidden)),
            "should not return Forbidden for own account"
        );
    }
}
