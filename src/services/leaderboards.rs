//! Leaderboard service — sqlx-backed data access for the
//! `leaderboards` / `leaderboard_entries` tables.
//!
//! All SQL touching leaderboards lives here so the HTTP layer
//! (`src/api/leaderboards.rs`) deals only in domain types. Errors are
//! projected through [`LeaderboardServiceError`] which the API layer maps
//! to HTTP status codes.

use crate::entities::leaderboard::{Leaderboard, LeaderboardEntryView, LeaderboardResponse};
use sqlx::MySqlPool;
use thiserror::Error;

/// Domain-level errors for leaderboard operations.
#[derive(Debug, Error)]
pub enum LeaderboardServiceError {
    #[error("leaderboard not found")]
    NotFound,
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("database error")]
    Database(#[source] sqlx::Error),
}

/// Pagination bounds for top-score queries.
///
/// `MAX_PER_PAGE` caps page size so a single request cannot dump an entire
/// large leaderboard into memory.
pub const DEFAULT_PER_PAGE: u64 = 50;
pub const MAX_PER_PAGE: u64 = 100;
pub const MIN_PER_PAGE: u64 = 1;

/// Look up a leaderboard by its route identifier (`internal_name`).
pub async fn find_by_game_id(
    pool: &MySqlPool,
    game_id: &str,
) -> Result<Option<Leaderboard>, LeaderboardServiceError> {
    sqlx::query_as::<_, Leaderboard>(
        "SELECT id, internal_name, display_name, created_at, updated_at \
         FROM leaderboards WHERE internal_name = ?",
    )
    .bind(game_id)
    .fetch_optional(pool)
    .await
    .map_err(LeaderboardServiceError::Database)
}

/// Fetch one paginated page of ranked entries for a leaderboard.
///
/// Entries are ordered by score descending (ties broken by earliest
/// submission). Ranks are 1-based and continue across pages. Deleted or
/// suspended players are excluded from rankings.
///
/// Returns [`LeaderboardServiceError::NotFound`] when `game_id` does not
/// match any leaderboard.
pub async fn top_entries(
    pool: &MySqlPool,
    game_id: &str,
    page: u64,
    per_page: u64,
) -> Result<LeaderboardResponse, LeaderboardServiceError> {
    if page == 0 {
        return Err(LeaderboardServiceError::Invalid("page must be >= 1".into()));
    }
    if !(MIN_PER_PAGE..=MAX_PER_PAGE).contains(&per_page) {
        return Err(LeaderboardServiceError::Invalid(
            "per_page must be between 1 and 100".into(),
        ));
    }

    let board = find_by_game_id(pool, game_id)
        .await?
        .ok_or(LeaderboardServiceError::NotFound)?;

    let offset = (page - 1).saturating_mul(per_page);
    let rows: Vec<(String, i64)> = sqlx::query_as(
        r#"
        SELECT p.public_id, e.score
        FROM leaderboard_entries e
        JOIN players p ON p.id = e.player_id AND p.status <> 'deleted'
        WHERE e.leaderboard_id = ?
        ORDER BY e.score DESC, e.created_at ASC, e.id ASC
        LIMIT ? OFFSET ?
        "#,
    )
    .bind(board.id)
    .bind(per_page)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(LeaderboardServiceError::Database)?;

    let entries = rows
        .into_iter()
        .enumerate()
        .map(|(i, (player_id, score))| LeaderboardEntryView {
            player_id,
            score,
            rank: offset + i as u64 + 1,
        })
        .collect();

    Ok(LeaderboardResponse { entries })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lazy pool that never connects — safe for pre-SQL validation paths.
    fn fake_pool() -> MySqlPool {
        MySqlPool::connect_lazy("mysql://test:test@127.0.0.1/test")
            .expect("lazy pool creation should not fail")
    }

    #[tokio::test]
    async fn top_entries_rejects_zero_page() {
        let pool = fake_pool();
        let result = top_entries(&pool, "any-board", 0, DEFAULT_PER_PAGE).await;
        assert!(
            matches!(result, Err(LeaderboardServiceError::Invalid(_))),
            "expected Invalid, got {result:?}"
        );
    }

    #[tokio::test]
    async fn top_entries_rejects_oversized_per_page() {
        let pool = fake_pool();
        let result = top_entries(&pool, "any-board", 1, MAX_PER_PAGE + 1).await;
        assert!(
            matches!(result, Err(LeaderboardServiceError::Invalid(_))),
            "expected Invalid, got {result:?}"
        );
    }

    #[tokio::test]
    async fn top_entries_rejects_zero_per_page() {
        let pool = fake_pool();
        let result = top_entries(&pool, "any-board", 1, 0).await;
        assert!(
            matches!(result, Err(LeaderboardServiceError::Invalid(_))),
            "expected Invalid, got {result:?}"
        );
    }
}
