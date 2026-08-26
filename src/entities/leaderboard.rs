//! `Leaderboard` entities — rows of the `leaderboards` /
//! `leaderboard_entries` tables and their public projections.
//!
//! See `migrations/20260825000001_create_leaderboards.up.sql` for the
//! canonical schema. The internal BIGINT ids are never exposed externally;
//! entries project the player's `public_id` instead.

use serde::Serialize;
use sqlx::FromRow;

/// Full leaderboard row, mirroring the `leaderboards` table 1:1.
#[derive(Debug, Clone, FromRow)]
pub struct Leaderboard {
    pub id: u64,
    pub internal_name: String,
    pub display_name: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// One ranked entry in a leaderboard response.
///
/// Shape follows the verified Talo surface (`docs/TALO_API.md`):
/// `{ "playerId": ..., "score": ..., "rank": ... }`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaderboardEntryView {
    /// Public UUID of the player who owns the score.
    pub player_id: String,
    /// Submitted score.
    pub score: i64,
    /// 1-based rank; continues across pages (page 2 starts at `per_page + 1`).
    pub rank: u64,
}

/// Response body for `GET /leaderboards/{game_id}`.
#[derive(Debug, Clone, Serialize)]
pub struct LeaderboardResponse {
    /// Ranked entries, highest score first.
    pub entries: Vec<LeaderboardEntryView>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(player_id: &str, score: i64, rank: u64) -> LeaderboardEntryView {
        LeaderboardEntryView {
            player_id: player_id.into(),
            score,
            rank,
        }
    }

    #[test]
    fn leaderboard_entry_view_serializes_camel_case() {
        let view = entry("player-uuid-1", 500, 1);

        let value = serde_json::to_value(&view).expect("serialize");
        let obj = value.as_object().expect("object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["playerId", "rank", "score"]);
        assert_eq!(obj["playerId"], "player-uuid-1");
        assert_eq!(obj["score"], 500);
        assert_eq!(obj["rank"], 1);
    }

    #[test]
    fn leaderboard_response_wraps_entries() {
        let resp = LeaderboardResponse {
            entries: vec![
                entry("player-uuid-1", 500, 1),
                entry("player-uuid-2", 100, 2),
            ],
        };

        let value = serde_json::to_value(&resp).expect("serialize");
        let obj = value.as_object().expect("object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["entries"]);
        let entries = obj["entries"].as_array().expect("entries array");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["playerId"], "player-uuid-1");
        assert_eq!(entries[0]["rank"], 1);
        assert_eq!(entries[1]["playerId"], "player-uuid-2");
    }
}
