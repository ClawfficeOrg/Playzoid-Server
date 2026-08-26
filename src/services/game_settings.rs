//! Game settings service — sqlx-backed data access for the `game_settings`
//! table.
//!
//! All SQL touching per-game configuration lives here so the HTTP layer
//! (`src/api/game_settings.rs`) deals only in domain types. Errors are
//! projected through [`GameSettingsServiceError`] which the API layer maps to
//! HTTP status codes.

use crate::entities::game_setting::{GameSettingRow, GameSettingView};
use sqlx::MySqlPool;
use thiserror::Error;

/// Domain-level errors for game-settings operations.
#[derive(Debug, Error)]
pub enum GameSettingsServiceError {
    /// No settings row exists for the requested game.
    #[error("game settings not found")]
    NotFound,
    /// Request input failed pre-SQL validation (id shape, null/oversized config).
    #[error("invalid input: {0}")]
    Invalid(String),
    /// The database operation failed.
    #[error("database error")]
    Database(#[source] sqlx::Error),
}

/// Maximum serialized size (bytes) accepted for a game's `config` JSON.
///
/// Mirrors the saves `MAX_SAVE_BYTES` cap: comfortably under InnoDB row-size
/// limits and consistent with the leaderboard `MAX_PROPS_BYTES` precedent of
/// enforcing size caps at the API layer (the JSON column itself cannot be
/// size-bounded in schema).
pub const MAX_CONFIG_BYTES: usize = 32 * 1024;

/// Read one game's stored configuration.
///
/// `game_id` is the opaque route identifier. A missing row maps to
/// [`GameSettingsServiceError::NotFound`] (HTTP 404).
pub async fn get_settings(
    pool: &MySqlPool,
    game_id: &str,
) -> Result<GameSettingView, GameSettingsServiceError> {
    read_back(pool, game_id).await
}

/// Store one game's configuration (upsert) and return the stored view.
///
/// Validation runs *before* any SQL so invalid requests can never touch the
/// database: the trimmed `game_id` must be 1..=64 characters (matching the
/// `VARCHAR(64)` column), `config` must not be JSON `null` (the column is NOT
/// NULL — a null would otherwise surface as a misleading 500), and the
/// serialized config must fit [`MAX_CONFIG_BYTES`].
///
/// The write is a parameterized upsert keyed on the unique `game_id`, so a
/// first PUT creates the row and subsequent PUTs replace only the `config`
/// column (`created_at` is preserved; MySQL maintains `updated_at` via
/// `ON UPDATE CURRENT_TIMESTAMP`). The row is read back and returned so
/// clients see the authoritative stored shape.
pub async fn put_settings(
    pool: &MySqlPool,
    game_id: &str,
    config: &serde_json::Value,
) -> Result<GameSettingView, GameSettingsServiceError> {
    let game_id = game_id.trim();
    if game_id.is_empty() || game_id.chars().count() > 64 {
        return Err(GameSettingsServiceError::Invalid(
            "game_id must be 1..=64 characters".into(),
        ));
    }
    if config.is_null() {
        return Err(GameSettingsServiceError::Invalid(
            "config must not be null".into(),
        ));
    }
    if serialized_len(config) > MAX_CONFIG_BYTES {
        return Err(GameSettingsServiceError::Invalid(
            "config exceeds the maximum size".into(),
        ));
    }

    sqlx::query(
        r#"
        INSERT INTO game_settings (game_id, config)
        VALUES (?, ?)
        ON DUPLICATE KEY UPDATE config = ?
        "#,
    )
    .bind(game_id)
    .bind(config.clone())
    .bind(config.clone())
    .execute(pool)
    .await
    .map_err(GameSettingsServiceError::Database)?;

    read_back(pool, game_id).await
}

/// Select one settings row by its opaque route identifier and project it.
///
/// The identifier is trimmed with the same rule as [`put_settings`] so a GET
/// always addresses the exact row a PUT to the identical URL created.
async fn read_back(
    pool: &MySqlPool,
    game_id: &str,
) -> Result<GameSettingView, GameSettingsServiceError> {
    let game_id = game_id.trim();
    let row = sqlx::query_as::<_, GameSettingRow>(
        r#"
        SELECT game_id, config, created_at, updated_at
        FROM game_settings
        WHERE game_id = ?
        "#,
    )
    .bind(game_id)
    .fetch_optional(pool)
    .await
    .map_err(GameSettingsServiceError::Database)?
    .ok_or(GameSettingsServiceError::NotFound)?;

    Ok(GameSettingView::from(row))
}

/// Serialized byte length of a JSON value, saturating if it cannot serialize.
fn serialized_len(value: &serde_json::Value) -> usize {
    serde_json::to_string(value)
        .map(|s| s.len())
        .unwrap_or(usize::MAX)
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
    fn error_display_and_source_chain() {
        assert_eq!(
            GameSettingsServiceError::NotFound.to_string(),
            "game settings not found"
        );
        assert_eq!(
            GameSettingsServiceError::Invalid("bad id".into()).to_string(),
            "invalid input: bad id"
        );
        let db_err = GameSettingsServiceError::Database(sqlx::Error::RowNotFound);
        assert_eq!(db_err.to_string(), "database error");
        assert!(
            std::error::Error::source(&db_err).is_some(),
            "Database variant must chain the underlying sqlx::Error"
        );
        assert!(
            std::error::Error::source(&GameSettingsServiceError::Invalid("x".into())).is_none(),
            "Invalid variant carries a plain message"
        );
    }

    #[tokio::test]
    async fn put_rejects_blank_game_id() {
        let pool = fake_pool();
        let result = put_settings(&pool, "   ", &serde_json::json!({"a": 1})).await;
        assert!(
            matches!(result, Err(GameSettingsServiceError::Invalid(_))),
            "expected Invalid, got {result:?}"
        );
    }

    #[tokio::test]
    async fn put_rejects_over_64_char_game_id() {
        // Boundary: exactly 64 chars would pass validation (DB required for
        // the happy path), so assert just past the boundary fails pre-SQL.
        let pool = fake_pool();
        let too_long = "g".repeat(65);
        let result = put_settings(&pool, &too_long, &serde_json::json!({"a": 1})).await;
        assert!(
            matches!(result, Err(GameSettingsServiceError::Invalid(_))),
            "expected Invalid, got {result:?}"
        );
    }

    #[tokio::test]
    async fn put_rejects_null_config() {
        let pool = fake_pool();
        let result = put_settings(&pool, "game-1", &serde_json::Value::Null).await;
        assert!(
            matches!(result, Err(GameSettingsServiceError::Invalid(_))),
            "expected Invalid, got {result:?}"
        );
    }

    #[tokio::test]
    async fn put_rejects_oversized_config() {
        let pool = fake_pool();
        // One string over the byte cap forces the size check to trip before
        // any SQL runs (fake pool never connects).
        let big = serde_json::json!({ "data": "x".repeat(MAX_CONFIG_BYTES) });
        let result = put_settings(&pool, "game-1", &big).await;
        assert!(
            matches!(result, Err(GameSettingsServiceError::Invalid(_))),
            "expected Invalid, got {result:?}"
        );
    }
}
