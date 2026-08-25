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
    #[error("player not found")]
    PlayerNotFound,
    #[error("entry not found")]
    EntryNotFound,
    #[error("an entry for this player already exists on this leaderboard")]
    Duplicate,
    #[error("forbidden: you may only modify your own entries")]
    Forbidden,
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

/// Maximum serialized size (bytes) accepted for the optional `props` blob.
pub const MAX_PROPS_BYTES: usize = 4096;

/// Submit a score for the authenticated player on a leaderboard.
///
/// One entry per player per leaderboard is enforced by the
/// `(leaderboard_id, player_id)` unique constraint — a re-submission maps to
/// [`LeaderboardServiceError::Duplicate`] so the API layer can return 409 and
/// direct clients to the PUT update endpoint (task 0.3-4).
///
/// Returns the stored view including the computed 1-based rank:
/// `1 + COUNT(entries with higher score OR equal score submitted earlier)`.
pub async fn submit_entry(
    pool: &MySqlPool,
    game_id: &str,
    player_public_id: &str,
    score: i64,
    props: Option<serde_json::Value>,
) -> Result<LeaderboardEntryView, LeaderboardServiceError> {
    validate_props(props.as_ref())?;

    let (board, player_internal_id) =
        resolve_board_and_player(pool, game_id, player_public_id).await?;

    let res = sqlx::query(
        "INSERT INTO leaderboard_entries (leaderboard_id, player_id, score, props) \
         VALUES (?, ?, ?, ?)",
    )
    .bind(board.id)
    .bind(player_internal_id)
    .bind(score)
    .bind(props)
    .execute(pool)
    .await;

    let entry_row = match res {
        Ok(r) => r.last_insert_id(),
        Err(sqlx::Error::Database(db_err)) if is_unique_violation(db_err.as_ref()) => {
            return Err(LeaderboardServiceError::Duplicate);
        }
        Err(e) => return Err(LeaderboardServiceError::Database(e)),
    };

    let rank = compute_rank(pool, board.id, score, entry_row).await?;
    Ok(LeaderboardEntryView {
        player_id: player_public_id.to_owned(),
        score,
        rank,
    })
}

/// Update the score (and optionally props) of an existing entry.
///
/// `player_public_id` must match the entry owner — cross-player updates map
/// to [`LeaderboardServiceError::Forbidden`]. The entry must already exist
/// ([`LeaderboardServiceError::EntryNotFound`] otherwise; use
/// [`submit_entry`] to create one). Omitted `props` keep their current value.
///
/// Returns the updated view including the recomputed 1-based rank.
pub async fn update_entry(
    pool: &MySqlPool,
    game_id: &str,
    player_public_id: &str,
    requesting_player_id: &str,
    score: i64,
    props: Option<serde_json::Value>,
) -> Result<LeaderboardEntryView, LeaderboardServiceError> {
    validate_props(props.as_ref())?;

    if player_public_id != requesting_player_id {
        return Err(LeaderboardServiceError::Forbidden);
    }

    let (board, player_internal_id) =
        resolve_board_and_player(pool, game_id, player_public_id).await?;

    // Fetch the existing entry — also its current props for omission passthrough.
    let existing: Option<(u64, Option<serde_json::Value>)> = sqlx::query_as(
        "SELECT id, props FROM leaderboard_entries \
         WHERE leaderboard_id = ? AND player_id = ?",
    )
    .bind(board.id)
    .bind(player_internal_id)
    .fetch_optional(pool)
    .await
    .map_err(LeaderboardServiceError::Database)?;
    let Some((entry_id, current_props)) = existing else {
        return Err(LeaderboardServiceError::EntryNotFound);
    };

    sqlx::query("UPDATE leaderboard_entries SET score = ?, props = ? WHERE id = ?")
        .bind(score)
        .bind(props.or(current_props))
        .bind(entry_id)
        .execute(pool)
        .await
        .map_err(LeaderboardServiceError::Database)?;

    let rank = compute_rank(pool, board.id, score, entry_id).await?;
    Ok(LeaderboardEntryView {
        player_id: player_public_id.to_owned(),
        score,
        rank,
    })
}

/// Validate the optional props blob: must be a JSON array within
/// [`MAX_PROPS_BYTES`] when serialised.
fn validate_props(props: Option<&serde_json::Value>) -> Result<(), LeaderboardServiceError> {
    let Some(p) = props else {
        return Ok(());
    };
    if !p.is_array() {
        return Err(LeaderboardServiceError::Invalid(
            "props must be a JSON array".into(),
        ));
    }
    if serde_json::to_string(p)
        .map(|s| s.len() > MAX_PROPS_BYTES)
        .unwrap_or(true)
    {
        return Err(LeaderboardServiceError::Invalid(
            "props exceeds maximum size".into(),
        ));
    }
    Ok(())
}

/// Resolve a leaderboard and a player by route identifiers.
async fn resolve_board_and_player(
    pool: &MySqlPool,
    game_id: &str,
    player_public_id: &str,
) -> Result<(Leaderboard, u64), LeaderboardServiceError> {
    let board = find_by_game_id(pool, game_id)
        .await?
        .ok_or(LeaderboardServiceError::NotFound)?;

    let player_id: Option<(u64,)> =
        sqlx::query_as("SELECT id FROM players WHERE public_id = ? AND status <> 'deleted'")
            .bind(player_public_id)
            .fetch_optional(pool)
            .await
            .map_err(LeaderboardServiceError::Database)?;
    let Some((player_internal_id,)) = player_id else {
        return Err(LeaderboardServiceError::PlayerNotFound);
    };

    Ok((board, player_internal_id))
}

/// 1-based rank of an entry: `1 + COUNT(higher scores OR equal scores with a
/// lower id, i.e. recorded earlier)`.
async fn compute_rank(
    pool: &MySqlPool,
    leaderboard_id: u64,
    score: i64,
    entry_id: u64,
) -> Result<u64, LeaderboardServiceError> {
    // COUNT(*) arrives as signed BIGINT on the MySQL wire protocol.
    let (better,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM leaderboard_entries \
         WHERE leaderboard_id = ? AND (score > ? OR (score = ? AND id < ?))",
    )
    .bind(leaderboard_id)
    .bind(score)
    .bind(score)
    .bind(entry_id)
    .fetch_one(pool)
    .await
    .map_err(LeaderboardServiceError::Database)?;

    Ok(better as u64 + 1)
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

    #[tokio::test]
    async fn submit_entry_rejects_non_array_props() {
        let pool = fake_pool();
        let result = submit_entry(
            &pool,
            "any-board",
            "player-uuid-1",
            100,
            Some(serde_json::json!({"not": "array"})),
        )
        .await;
        assert!(
            matches!(result, Err(LeaderboardServiceError::Invalid(_))),
            "expected Invalid, got {result:?}"
        );
    }

    #[tokio::test]
    async fn submit_entry_rejects_oversized_props() {
        let pool = fake_pool();
        // One array element over the byte cap forces the size check to trip
        // before any SQL runs (fake pool never connects).
        let big =
            serde_json::Value::Array(vec![serde_json::Value::String("x".into()); MAX_PROPS_BYTES]);
        let result = submit_entry(&pool, "any-board", "player-uuid-1", 100, Some(big)).await;
        assert!(
            matches!(result, Err(LeaderboardServiceError::Invalid(_))),
            "expected Invalid, got {result:?}"
        );
    }

    #[tokio::test]
    async fn update_entry_rejects_cross_player() {
        let pool = fake_pool();
        let result = update_entry(&pool, "any-board", "player-A", "player-B", 100, None).await;
        assert!(
            matches!(result, Err(LeaderboardServiceError::Forbidden)),
            "expected Forbidden, got {result:?}"
        );
    }

    #[tokio::test]
    async fn update_entry_same_player_passes_ownership_check() {
        // Ownership check passes, then we hit the (non-existent) fake DB and
        // get a Database error — anything but Forbidden.
        let pool = fake_pool();
        let result = update_entry(&pool, "any-board", "player-A", "player-A", 100, None).await;
        assert!(
            !matches!(
                result,
                Err(LeaderboardServiceError::Forbidden) | Err(LeaderboardServiceError::Invalid(_))
            ),
            "should pass validation for own entry, got {result:?}"
        );
    }
}
