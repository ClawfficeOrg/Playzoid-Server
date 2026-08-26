//! Saves service — sqlx-backed data access for the `game_saves` table.
//!
//! All SQL touching game saves lives here so the HTTP layer
//! (`src/api/saves.rs`) deals only in domain types. Errors are projected
//! through [`SaveServiceError`] which the API layer maps to HTTP status codes.

use crate::entities::save::{SaveRow, SaveView};
use sqlx::MySqlPool;
use thiserror::Error;

/// Domain-level errors for game-save operations.
#[derive(Debug, Error)]
pub enum SaveServiceError {
    #[error("player not found")]
    NotFound,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_service_error_messages() {
        assert_eq!(SaveServiceError::NotFound.to_string(), "player not found");
        let db_err = SaveServiceError::Database(sqlx::Error::RowNotFound);
        assert_eq!(db_err.to_string(), "database error");
        assert!(
            std::error::Error::source(&db_err).is_some(),
            "Database variant must chain the underlying sqlx::Error"
        );
    }
}
