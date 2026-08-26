//! Saves service — sqlx-backed data access for the `game_saves` table.
//!
//! All SQL touching game saves lives here so the HTTP layer
//! (`src/api/saves.rs`) deals only in domain types. Errors are projected
//! through [`SaveServiceError`] which the API layer maps to HTTP status codes.

use crate::entities::save::{SaveRow, SaveView};
use sqlx::MySqlPool;
use thiserror::Error;
use uuid::Uuid;

/// Domain-level errors for game-save operations.
#[derive(Debug, Error)]
pub enum SaveServiceError {
    #[error("player not found")]
    NotFound,
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("database error")]
    Database(#[source] sqlx::Error),
}

/// List every save owned by `player_public_id`, newest first.
///
/// The player is resolved to its internal id first and must not be
/// soft-deleted — a missing or deleted player maps to
/// [`SaveServiceError::NotFound`]. A player with no saves yields an empty
/// `Vec` (HTTP 200 `[]`).
pub async fn list_saves(
    pool: &MySqlPool,
    player_public_id: &str,
) -> Result<Vec<SaveView>, SaveServiceError> {
    let player_id: Option<(u64,)> =
        sqlx::query_as("SELECT id FROM players WHERE public_id = ? AND status <> 'deleted'")
            .bind(player_public_id)
            .fetch_optional(pool)
            .await
            .map_err(SaveServiceError::Database)?;

    let Some((player_id,)) = player_id else {
        return Err(SaveServiceError::NotFound);
    };

    let rows = sqlx::query_as::<_, SaveRow>(
        r#"
        SELECT public_id, name, save, metadata, created_at, updated_at
        FROM game_saves
        WHERE player_id = ?
        ORDER BY created_at DESC, updated_at DESC
        "#,
    )
    .bind(player_id)
    .fetch_all(pool)
    .await
    .map_err(SaveServiceError::Database)?;

    let views = rows
        .into_iter()
        .map(|row| SaveView::from((row, player_public_id.to_owned())))
        .collect();

    Ok(views)
}

/// Maximum combined serialized size (bytes) accepted for `save` + `metadata`.
///
/// 32 KiB keeps rows comfortably under InnoDB's 65,535-byte row-size limit
/// (with the 255-char `name` in `utf8mb4` worst case consuming the rest),
/// mirroring the leaderboard `MAX_PROPS_BYTES` size-cap precedent.
pub const MAX_SAVE_BYTES: usize = 32 * 1024;

/// Create a game save for the owning player and return its stored view.
///
/// The player is resolved to its internal id first and must not be
/// soft-deleted — a missing or deleted player maps to
/// [`SaveServiceError::NotFound`]. Validation runs *before* any SQL:
/// the trimmed `name` must be 1..=255 chars, `save` must not be JSON `null`
/// (the column is NOT NULL and a null would otherwise surface as a misleading
/// 500), and the combined serialized `save` + `metadata` must fit
/// [`MAX_SAVE_BYTES`].
///
/// The save's `public_id` is a fresh UUID v4; on the astronomically unlikely
/// event of a collision the insert is retried once with a new UUID before the
/// unique violation is surfaced as a database error.
pub async fn create_save(
    pool: &MySqlPool,
    player_public_id: &str,
    name: &str,
    save: &serde_json::Value,
    metadata: Option<&serde_json::Value>,
) -> Result<SaveView, SaveServiceError> {
    let name = name.trim();
    if name.is_empty() || name.len() > 255 {
        return Err(SaveServiceError::Invalid(
            "name must be 1..=255 characters".into(),
        ));
    }
    if save.is_null() {
        return Err(SaveServiceError::Invalid("save must not be null".into()));
    }
    let save_len = serialized_len(save);
    let metadata_len = metadata.map(serialized_len).unwrap_or_default();
    if save_len.saturating_add(metadata_len) > MAX_SAVE_BYTES {
        return Err(SaveServiceError::Invalid(
            "save and metadata combined exceed the maximum size".into(),
        ));
    }

    let player_id: Option<(u64,)> =
        sqlx::query_as("SELECT id FROM players WHERE public_id = ? AND status <> 'deleted'")
            .bind(player_public_id)
            .fetch_optional(pool)
            .await
            .map_err(SaveServiceError::Database)?;

    let Some((player_id,)) = player_id else {
        return Err(SaveServiceError::NotFound);
    };

    let mut public_id = Uuid::new_v4().to_string();
    let mut saved_public_id = None;

    // Retry once on a UUID collision before surfacing the unique violation.
    for _ in 0..2 {
        match sqlx::query(
            "INSERT INTO game_saves (public_id, player_id, name, save, metadata) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&public_id)
        .bind(player_id)
        .bind(name)
        .bind(save.clone())
        .bind(metadata.cloned())
        .execute(pool)
        .await
        {
            Ok(_) => {
                saved_public_id = Some(public_id);
                break;
            }
            Err(sqlx::Error::Database(db_err)) if is_unique_violation(db_err.as_ref()) => {
                tracing::warn!(error = %db_err, "save public_id collision; retrying");
                public_id = Uuid::new_v4().to_string();
            }
            Err(e) => return Err(SaveServiceError::Database(e)),
        }
    }

    let public_id = saved_public_id.ok_or_else(|| {
        SaveServiceError::Database(sqlx::Error::Protocol(
            "save public_id collision retry exhausted".into(),
        ))
    })?;

    read_back_save(pool, &public_id, player_public_id).await
}

/// Read one save back by its `public_id` and project it to a view for the
/// owning player.
async fn read_back_save(
    pool: &MySqlPool,
    public_id: &str,
    player_public_id: &str,
) -> Result<SaveView, SaveServiceError> {
    let row = sqlx::query_as::<_, SaveRow>(
        r#"
        SELECT public_id, name, save, metadata, created_at, updated_at
        FROM game_saves
        WHERE public_id = ?
        "#,
    )
    .bind(public_id)
    .fetch_optional(pool)
    .await
    .map_err(SaveServiceError::Database)?
    .ok_or(SaveServiceError::NotFound)?;

    Ok(SaveView::from((row, player_public_id.to_owned())))
}

/// Serialized byte length of a JSON value, saturating if it cannot serialize.
fn serialized_len(value: &serde_json::Value) -> usize {
    serde_json::to_string(value)
        .map(|s| s.len())
        .unwrap_or(usize::MAX)
}

/// MySQL `ER_DUP_ENTRY` — SQLSTATE 23000 / "duplicate entry".
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

    /// Lazy pool that never connects — safe for pre-SQL validation paths.
    fn fake_pool() -> MySqlPool {
        MySqlPool::connect_lazy("mysql://test:test@127.0.0.1/test")
            .expect("lazy pool creation should not fail")
    }

    #[test]
    fn save_service_error_messages() {
        assert_eq!(SaveServiceError::NotFound.to_string(), "player not found");
        assert_eq!(
            SaveServiceError::Invalid("bad input".into()).to_string(),
            "invalid input: bad input"
        );
        let db_err = SaveServiceError::Database(sqlx::Error::RowNotFound);
        assert_eq!(db_err.to_string(), "database error");
        assert!(
            std::error::Error::source(&db_err).is_some(),
            "Database variant must chain the underlying sqlx::Error"
        );
        assert!(
            std::error::Error::source(&SaveServiceError::Invalid("x".into())).is_none(),
            "Invalid variant carries a plain message"
        );
    }

    #[tokio::test]
    async fn create_save_rejects_blank_name() {
        let pool = fake_pool();
        let result = create_save(
            &pool,
            "player-uuid-1",
            "   ",
            &serde_json::json!({"hp": 100}),
            None,
        )
        .await;
        assert!(
            matches!(result, Err(SaveServiceError::Invalid(_))),
            "expected Invalid, got {result:?}"
        );
    }

    #[tokio::test]
    async fn create_save_rejects_null_save() {
        let pool = fake_pool();
        let result = create_save(
            &pool,
            "player-uuid-1",
            "slot-1",
            &serde_json::Value::Null,
            None,
        )
        .await;
        assert!(
            matches!(result, Err(SaveServiceError::Invalid(_))),
            "expected Invalid, got {result:?}"
        );
    }

    #[tokio::test]
    async fn create_save_rejects_oversized_blob() {
        let pool = fake_pool();
        // One string over the byte cap forces the size check to trip before
        // any SQL runs (fake pool never connects).
        let big = serde_json::json!({ "data": "x".repeat(MAX_SAVE_BYTES) });
        let result = create_save(&pool, "player-uuid-1", "slot-1", &big, None).await;
        assert!(
            matches!(result, Err(SaveServiceError::Invalid(_))),
            "expected Invalid, got {result:?}"
        );
    }

    #[tokio::test]
    async fn create_save_rejects_oversized_metadata() {
        let pool = fake_pool();
        // save fits alone, but the metadata pushes the combined size over
        // the cap — validation must trip before any SQL.
        let save = serde_json::json!({ "hp": 100 });
        let metadata = serde_json::json!({ "zone": "x".repeat(MAX_SAVE_BYTES) });
        let result = create_save(&pool, "player-uuid-1", "slot-1", &save, Some(&metadata)).await;
        assert!(
            matches!(result, Err(SaveServiceError::Invalid(_))),
            "expected Invalid, got {result:?}"
        );
    }
}
