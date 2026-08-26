//! `Save` entities — rows of the `game_saves` table and their public projections.
//!
//! See `migrations/20260825000002_create_game_saves.up.sql` for the canonical
//! schema and `docs/TALO_API.md` → game saves for the public API mapping. The
//! internal `id` (BIGINT) is *never* exposed externally; saves surface their
//! `public_id` (UUID) as `id` instead, mirroring the `players` convention.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;

/// Public projection of a `game_saves` row — the shape returned by HTTP endpoints.
///
/// Critically omits the internal `id` and `player_id` BIGINTs; exposes the
/// save's `public_id` as `id` and the owning player's `public_id` as `playerId`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveView {
    /// Public UUID of the save.
    pub id: String,
    /// Public UUID of the owning player.
    pub player_id: String,
    /// Human-readable save name.
    pub name: String,
    /// Arbitrary game-state JSON blob.
    pub save: serde_json::Value,
    /// Optional game-specific metadata.
    pub metadata: Option<serde_json::Value>,
    /// When the save was created.
    pub created_at: DateTime<Utc>,
    /// When the save was last updated.
    pub updated_at: DateTime<Utc>,
}

/// Internal row of the `game_saves` table — never exposed on the wire.
///
/// Only the columns needed for the listing endpoint are selected; the internal
/// `id` is intentionally not read so list ordering never depends on it.
#[derive(Debug, Clone, FromRow)]
pub(crate) struct SaveRow {
    pub(crate) public_id: String,
    pub(crate) name: String,
    pub(crate) save: serde_json::Value,
    pub(crate) metadata: Option<serde_json::Value>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

impl From<(SaveRow, String)> for SaveView {
    fn from((row, player_id): (SaveRow, String)) -> Self {
        Self {
            id: row.public_id,
            player_id,
            name: row.name,
            save: row.save,
            metadata: row.metadata,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_row_converts_to_view() {
        let created_at: DateTime<Utc> = "2026-08-25T10:00:00Z".parse().expect("parse");
        let updated_at: DateTime<Utc> = "2026-08-25T11:00:00Z".parse().expect("parse");
        let row = SaveRow {
            public_id: "save-uuid-1".into(),
            name: "slot-1".into(),
            save: serde_json::json!({"level": 3, "hp": 42}),
            metadata: Some(serde_json::json!({"zone": "level1"})),
            created_at,
            updated_at,
        };

        let view = SaveView::from((row, "player-uuid-1".into()));

        assert_eq!(view.id, "save-uuid-1");
        assert_eq!(view.player_id, "player-uuid-1");
        assert_eq!(view.name, "slot-1");
        assert_eq!(view.save, serde_json::json!({"level": 3, "hp": 42}));
        assert_eq!(view.metadata, Some(serde_json::json!({"zone": "level1"})));
        assert_eq!(view.created_at, created_at);
        assert_eq!(view.updated_at, updated_at);
    }

    #[test]
    fn save_view_serializes_camel_case() {
        let view = SaveView {
            id: "save-uuid-1".into(),
            player_id: "player-uuid-1".into(),
            name: "slot-1".into(),
            save: serde_json::json!({"hp": 100}),
            metadata: None,
            created_at: "2026-08-25T10:00:00Z".parse().expect("parse"),
            updated_at: "2026-08-25T10:00:00Z".parse().expect("parse"),
        };

        let value = serde_json::to_value(&view).expect("serialize");
        let obj = value.as_object().expect("object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "createdAt",
                "id",
                "metadata",
                "name",
                "playerId",
                "save",
                "updatedAt"
            ]
        );
        assert_eq!(obj["metadata"], serde_json::Value::Null);
    }
}
