//! `Leaderboard` entities — rows of the `leaderboards` /
//! `leaderboard_entries` tables and their public projections.
//!
//! See `migrations/20260825000001_create_leaderboards.up.sql` for the
//! canonical schema. The internal BIGINT ids are never exposed externally;
//! entries project the player's `public_id` instead.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::entities::player_alias::PlayerAlias;
use crate::entities::prop::Prop;

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

/// Upstream sort mode of a leaderboard (`"asc"` / `"desc"` on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LeaderboardSortMode {
    /// Lowest score ranks first.
    Asc,
    /// Highest score ranks first.
    Desc,
}

/// Full upstream-parity `LeaderboardEntry` (verified against the leaderboard
/// API samples; mapped in `docs/TALO_API.md` → domain models).
///
/// Distinct from [`LeaderboardEntryView`] — the view is Playzoid's own
/// implemented response shape (`{playerId, score, rank}`, `i64` scores from
/// our BIGINT column), while this struct mirrors the upstream entry 1:1,
/// including the nested [`PlayerAlias`] and upstream `props`. Not yet
/// produced by any endpoint; it is the parity target for future work.
///
/// Divergences from upstream, kept deliberately:
/// - `score` is `f64` here to deserialize upstream's numeric samples
///   (`593.21`) exactly; our persisted schema stays `BIGINT i64`.
/// - `position` is upstream's 0-based index; Playzoid's own views expose a
///   1-based `rank` instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaderboardEntry {
    /// Internal entry id (upstream exposes this number publicly).
    pub id: i64,
    /// 0-based position in the leaderboard's sort order.
    pub position: u64,
    /// Submitted score. `f64` for upstream parity (upstream samples carry
    /// floats); Playzoid's own storage remains `i64`.
    pub score: f64,
    /// Display name of the owning leaderboard.
    pub leaderboard_name: String,
    /// Route identifier of the owning leaderboard (our `internal_name`).
    pub leaderboard_internal_name: String,
    /// Sort order of the owning leaderboard.
    pub leaderboard_sort_mode: LeaderboardSortMode,
    /// Alias that submitted the score (nested player projection included).
    pub player_alias: PlayerAlias,
    /// Whether the entry is hidden from public boards.
    pub hidden: bool,
    /// Props attached to the submission; omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub props: Vec<Prop>,
    /// When the entry was created.
    pub created_at: DateTime<Utc>,
    /// When the entry was last updated.
    pub updated_at: DateTime<Utc>,
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

    // ---- Full upstream-parity LeaderboardEntry (task 0.4.2) ----

    fn sample_entry() -> LeaderboardEntry {
        let alias = PlayerAlias {
            id: 1,
            service: "steam".into(),
            identifier: "11133645".into(),
            display_name: Some("11133645".into()),
            player: crate::entities::player_alias::PlayerRef {
                id: "7a4e70ec-6ee6-418e-923d-b3a45051b7f9".into(),
                props: vec![Prop {
                    key: "xPos".into(),
                    value: "13.29".into(),
                }],
                dev_build: false,
                last_seen_at: "2022-04-12T15:09:43.066Z".parse().expect("parse ts"),
                created_at: "2022-01-15T13:20:32.133Z".parse().expect("parse ts"),
                auth: None,
            },
            last_seen_at: Some("2024-12-04T07:15:13.000Z".parse().expect("parse ts")),
            created_at: Some("2024-10-25T18:18:28.000Z".parse().expect("parse ts")),
            updated_at: Some("2024-12-04T07:15:13.000Z".parse().expect("parse ts")),
        };

        LeaderboardEntry {
            id: 4,
            position: 0,
            score: 593.21,
            leaderboard_name: "Highscore".into(),
            leaderboard_internal_name: "highscore".into(),
            leaderboard_sort_mode: LeaderboardSortMode::Asc,
            player_alias: alias,
            hidden: false,
            props: vec![Prop {
                key: "level".into(),
                value: "3".into(),
            }],
            created_at: "2022-01-15T14:01:18.727Z".parse().expect("parse ts"),
            updated_at: "2022-02-16T16:03:53.123Z".parse().expect("parse ts"),
        }
    }

    #[test]
    fn leaderboard_sort_mode_serializes_lowercase() {
        assert_eq!(
            serde_json::to_value(LeaderboardSortMode::Asc).expect("serialize"),
            serde_json::json!("asc")
        );
        assert_eq!(
            serde_json::to_value(LeaderboardSortMode::Desc).expect("serialize"),
            serde_json::json!("desc")
        );

        let back: LeaderboardSortMode =
            serde_json::from_value(serde_json::json!("desc")).expect("deserialize");
        assert_eq!(back, LeaderboardSortMode::Desc);

        let rejected: Result<LeaderboardSortMode, _> =
            serde_json::from_value(serde_json::json!("sideways"));
        assert!(rejected.is_err(), "unknown sort modes must be rejected");
    }

    #[test]
    fn full_leaderboard_entry_matches_upstream_keys() {
        let value = serde_json::to_value(sample_entry()).expect("serialize");
        let obj = value.as_object().expect("object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "createdAt",
                "hidden",
                "id",
                "leaderboardInternalName",
                "leaderboardName",
                "leaderboardSortMode",
                "playerAlias",
                "position",
                "props",
                "score",
                "updatedAt"
            ]
        );
        assert_eq!(obj["leaderboardSortMode"], "asc");
        assert_eq!(obj["score"], 593.21);
        assert_eq!(obj["props"][0]["key"], "level");
    }

    #[test]
    fn full_leaderboard_entry_omits_empty_props() {
        let mut entry = sample_entry();
        entry.props.clear();

        let value = serde_json::to_value(&entry).expect("serialize");
        assert!(value.get("props").is_none());
    }

    /// Fixture lifted verbatim from the leaderboard API entries sample
    /// response (docs.trytalo.com/docs/http/leaderboard-api), including the
    /// float score and the circular `aliases` marker upstream emits.
    #[test]
    fn full_leaderboard_entry_roundtrips_upstream_sample() {
        let fixture = serde_json::json!({
            "id": 4,
            "position": 0,
            "score": 593.21,
            "leaderboardName": "Highscore",
            "leaderboardInternalName": "highscore",
            "leaderboardSortMode": "asc",
            "playerAlias": {
                "id": 1,
                "service": "steam",
                "identifier": "11133645",
                "displayName": "11133645",
                "player": {
                    "id": "7a4e70ec-6ee6-418e-923d-b3a45051b7f9",
                    "props": [
                        {"key": "xPos", "value": "13.29"},
                        {"key": "yPos", "value": "26.44"}
                    ],
                    "aliases": ["/* [Circular] */"],
                    "devBuild": false,
                    "createdAt": "2022-01-15T13:20:32.133Z",
                    "lastSeenAt": "2022-04-12T15:09:43.066Z"
                }
            },
            "hidden": false,
            "createdAt": "2022-01-15T14:01:18.727Z",
            "updatedAt": "2022-01-15T14:01:18.727Z"
        });

        let entry: LeaderboardEntry = serde_json::from_value(fixture).expect("deserialize");
        assert_eq!(entry.id, 4);
        assert_eq!(entry.position, 0);
        assert!((entry.score - 593.21).abs() < f64::EPSILON);
        assert_eq!(entry.leaderboard_sort_mode, LeaderboardSortMode::Asc);
        assert!(!entry.hidden);
        // Entry-level props absent in the sample -> empty vec.
        assert!(entry.props.is_empty());
        // Nested player props preserved through deserialize.
        assert_eq!(entry.player_alias.player.props.len(), 2);
        assert_eq!(entry.player_alias.player.props[0].key, "xPos");

        // Re-serialization preserves the payload (props included).
        let round = serde_json::to_value(&entry).expect("re-serialize");
        assert_eq!(
            round["playerAlias"]["player"]["props"][0],
            serde_json::json!({"key": "xPos", "value": "13.29"})
        );
        assert_eq!(round["score"], serde_json::json!(593.21));
    }
}
