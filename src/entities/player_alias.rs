//! `PlayerAlias` domain model — the upstream identity a player uses inside a
//! game (one row per `(service, identifier)` pair), plus the nested `Player`
//! projection it hangs off.
//!
//! Shapes verified against the Talo socket reference types and the live
//! HTTP samples (game-channel API owner/member payloads, leaderboard API
//! entries); mapped in `docs/TALO_API.md` → domain models.
//!
//! Deliberate divergences from upstream:
//! - Upstream's circular `Player.aliases` back-reference is omitted — it
//!   recurses into the very alias holding the player and is always
//!   `[Circular]` in upstream samples.
//! - Upstream's `Player.groups` (`{ id, name }[]`) is not modelled yet;
//!   Playzoid has no player-group persistence.
//! - Alias-level timestamps are `Option` because upstream serializes them in
//!   channel payloads but omits them inside leaderboard entry samples.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::entities::player_auth::PlayerAuth;
use crate::entities::prop::Prop;

/// Upstream `Player` as nested inside a `PlayerAlias`.
///
/// This is the *projection* of the account, not our internal `players` row:
/// `id` is the public UUID string on every verified HTTP sample (the socket
/// reference still types it as a number on legacy flows) and no credential
/// material ever appears here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerRef {
    /// Public UUID of the owning account.
    pub id: String,
    /// Free-form props attached to the player by the game.
    #[serde(default)]
    pub props: Vec<Prop>,
    /// Whether the player was created against a dev build of the game.
    pub dev_build: bool,
    /// When the player was last seen online.
    pub last_seen_at: DateTime<Utc>,
    /// When the player account was created.
    pub created_at: DateTime<Utc>,
    /// Email-verification state; absent for anonymous aliases.
    #[serde(default)]
    pub auth: Option<PlayerAuth>,
}

/// Upstream `PlayerAlias`: one identity of a player under one login service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerAlias {
    /// Internal alias id (upstream exposes this number publicly).
    pub id: i64,
    /// Login service backing the alias (`"username"`, `"steam"`, ...).
    pub service: String,
    /// Service-scoped identifier (steam id, username, email, ...).
    pub identifier: String,
    /// Optional display name; omitted from the wire when unset.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub display_name: Option<String>,
    /// The player account the alias belongs to.
    pub player: PlayerRef,
    /// Last-seen timestamp for the alias (`null` when upstream omits it).
    #[serde(default)]
    pub last_seen_at: Option<DateTime<Utc>>,
    /// Creation timestamp for the alias (`null` when upstream omits it).
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    /// Last-update timestamp for the alias (`null` when upstream omits it).
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_player_ref() -> PlayerRef {
        PlayerRef {
            id: "7a4e70ec-6ee6-418e-923d-b3a45051b7f9".into(),
            props: vec![Prop {
                key: "xPos".into(),
                value: "13.29".into(),
            }],
            dev_build: false,
            last_seen_at: "2022-04-12T15:09:43.066Z".parse().expect("parse ts"),
            created_at: "2022-01-15T13:20:32.133Z".parse().expect("parse ts"),
            auth: None,
        }
    }

    fn sample_alias() -> PlayerAlias {
        PlayerAlias {
            id: 1,
            service: "steam".into(),
            identifier: "11133645".into(),
            display_name: Some("11133645".into()),
            player: sample_player_ref(),
            last_seen_at: Some("2024-12-04T07:15:13.000Z".parse().expect("parse ts")),
            created_at: Some("2024-10-25T18:18:28.000Z".parse().expect("parse ts")),
            updated_at: Some("2024-12-04T07:15:13.000Z".parse().expect("parse ts")),
        }
    }

    #[test]
    fn player_alias_serializes_camel_case_full_shape() {
        let value = serde_json::to_value(sample_alias()).expect("serialize");
        let obj = value.as_object().expect("object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "createdAt",
                "displayName",
                "id",
                "identifier",
                "lastSeenAt",
                "player",
                "service",
                "updatedAt"
            ]
        );
        assert_eq!(obj["service"], "steam");
        let player = obj["player"].as_object().expect("player object");
        assert_eq!(player["id"], "7a4e70ec-6ee6-418e-923d-b3a45051b7f9");
        assert_eq!(player["devBuild"], false);
    }

    #[test]
    fn player_alias_omits_display_name_when_none() {
        let mut alias = sample_alias();
        alias.display_name = None;

        let value = serde_json::to_value(&alias).expect("serialize");
        assert!(value.get("displayName").is_none());
    }

    #[test]
    fn player_alias_never_exposes_password() {
        let serialized = serde_json::to_string(&sample_alias()).expect("serialize");
        assert!(
            !serialized.to_lowercase().contains("password"),
            "PlayerAlias must never carry credential material: {serialized}"
        );
    }

    /// Fixture lifted verbatim from the game-channel API "list members"
    /// sample response (docs.trytalo.com/docs/http/game-channel-api).
    #[test]
    fn player_alias_deserializes_from_upstream_fixture() {
        let fixture = serde_json::json!({
            "id": 105,
            "service": "username",
            "identifier": "player_one",
            "displayName": "player_one",
            "player": {
                "id": "85d67584-1346-4fad-a17f-fd7bd6c85364",
                "props": [],
                "devBuild": false,
                "createdAt": "2025-04-25T18:18:28.000Z",
                "lastSeenAt": "2025-05-04T07:15:13.000Z",
                "groups": []
            },
            "lastSeenAt": "2025-05-04T07:15:13.000Z",
            "createdAt": "2025-04-25T18:18:28.000Z",
            "updatedAt": "2025-05-04T07:15:13.000Z"
        });

        let alias: PlayerAlias = serde_json::from_value(fixture).expect("deserialize");
        assert_eq!(alias.id, 105);
        assert_eq!(alias.service, "username");
        assert_eq!(alias.identifier, "player_one");
        assert_eq!(alias.display_name.as_deref(), Some("player_one"));
        assert_eq!(alias.player.id, "85d67584-1346-4fad-a17f-fd7bd6c85364");
        assert!(alias.player.props.is_empty());
        assert!(!alias.player.dev_build);
        assert!(alias.last_seen_at.is_some());
        // Unknown upstream fields ("groups") are ignored, not errors.
    }

    /// The leaderboard API samples nest the alias *without* its timestamps;
    /// those must deserialize to `None` rather than fail.
    #[test]
    fn player_alias_tolerates_missing_timestamps() {
        let fixture = serde_json::json!({
            "id": 1,
            "service": "steam",
            "identifier": "11133645",
            "displayName": "11133645",
            "player": {
                "id": "7a4e70ec-6ee6-418e-923d-b3a45051b7f9",
                "props": [],
                "devBuild": false,
                "createdAt": "2022-01-15T13:20:32.133Z",
                "lastSeenAt": "2022-04-12T15:09:43.066Z"
            }
        });

        let alias: PlayerAlias = serde_json::from_value(fixture).expect("deserialize");
        assert_eq!(alias.last_seen_at, None);
        assert_eq!(alias.created_at, None);
        assert_eq!(alias.updated_at, None);
    }

    #[test]
    fn player_alias_deserializes_nested_auth() {
        let fixture = serde_json::json!({
            "id": 9,
            "service": "username",
            "identifier": "alice",
            "player": {
                "id": "85d67584-1346-4fad-a17f-fd7bd6c85364",
                "props": [],
                "devBuild": false,
                "createdAt": "2025-04-25T18:18:28.000Z",
                "lastSeenAt": "2025-05-04T07:15:13.000Z",
                "auth": {
                    "email": "alice@example.com",
                    "verificationEnabled": true,
                    "sessionCreatedAt": "2025-05-04T07:00:00.000Z"
                }
            },
            "lastSeenAt": "2025-05-04T07:15:13.000Z",
            "createdAt": "2025-04-25T18:18:28.000Z",
            "updatedAt": "2025-05-04T07:15:13.000Z"
        });

        let alias: PlayerAlias = serde_json::from_value(fixture).expect("deserialize");
        let auth = alias.player.auth.expect("auth present");
        assert_eq!(auth.email.as_deref(), Some("alice@example.com"));
        assert!(auth.verification_enabled);
    }
}
