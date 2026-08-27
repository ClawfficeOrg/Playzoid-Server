//! `GameSetting` entities — rows of the `game_settings` table and their public
//! projection.
//!
//! See `migrations/20260826000001_create_game_settings.up.sql` for the
//! canonical schema and `docs/TALO_API.md` → game config for the public API
//! mapping. Games are addressed externally by their opaque `game_id` route
//! identifier (mirroring the leaderboards' `internal_name` convention); the
//! internal BIGINT `id` is *never* selected or exposed, mirroring the
//! `players`/`game_saves` convention.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;

/// Public projection of a `game_settings` row — the shape returned by the
/// settings HTTP endpoints.
///
/// Omits the internal `id` BIGINT; exposes the opaque route identifier as
/// `gameId` and the arbitrary per-game JSON `config`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameSettingView {
    /// Opaque route identifier of the game.
    pub game_id: String,
    /// Arbitrary per-game configuration JSON.
    pub config: serde_json::Value,
    /// When the settings row was created (first PUT).
    pub created_at: DateTime<Utc>,
    /// When the configuration was last written.
    pub updated_at: DateTime<Utc>,
}

/// Internal row of the `game_settings` table — never exposed on the wire.
///
/// Only the columns needed by the read-back projection are selected; the
/// internal `id` is intentionally not read so nothing can depend on it.
#[derive(Debug, Clone, FromRow)]
pub(crate) struct GameSettingRow {
    pub(crate) game_id: String,
    pub(crate) config: serde_json::Value,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

impl From<GameSettingRow> for GameSettingView {
    fn from(row: GameSettingRow) -> Self {
        Self {
            game_id: row.game_id,
            config: row.config,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_setting_row_converts_to_view() {
        let created_at: DateTime<Utc> = "2026-08-26T10:00:00Z".parse().expect("parse");
        let updated_at: DateTime<Utc> = "2026-08-26T11:00:00Z".parse().expect("parse");
        let row = GameSettingRow {
            game_id: "game-abc-1".into(),
            config: serde_json::json!({"difficulty": "hard", "lives": 3}),
            created_at,
            updated_at,
        };

        let view = GameSettingView::from(row);

        assert_eq!(view.game_id, "game-abc-1");
        assert_eq!(
            view.config,
            serde_json::json!({"difficulty": "hard", "lives": 3})
        );
        assert_eq!(view.created_at, created_at);
        assert_eq!(view.updated_at, updated_at);
    }

    #[test]
    fn game_setting_view_serializes_camel_case() {
        let view = GameSettingView {
            game_id: "game-abc-1".into(),
            config: serde_json::json!({"difficulty": "hard"}),
            created_at: "2026-08-26T10:00:00Z".parse().expect("parse"),
            updated_at: "2026-08-26T10:00:00Z".parse().expect("parse"),
        };

        let value = serde_json::to_value(&view).expect("serialize");
        let obj = value.as_object().expect("object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["config", "createdAt", "gameId", "updatedAt"]);
        // The internal BIGINT id must never appear on the wire.
        assert!(obj.get("id").is_none());
    }
}
