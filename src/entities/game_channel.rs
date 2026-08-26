//! `GameChannel` domain model — upstream Talo game channels (chat rooms /
//! shared state) plus the channel leaving-reason enum used by the
//! `v1.channels.player-left` envelope.
//!
//! Shapes verified against the game-channel API samples and the socket
//! reference (see `docs/TALO_API.md` → domain models). Playzoid does not
//! persist channels server-side yet (membership lives in the in-memory
//! `ChannelHub`); these structs are the parity target for future endpoints.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::entities::player_alias::PlayerAlias;
use crate::entities::prop::Prop;

/// Upstream `GameChannel`: a named, prop-carrying chat/state room owned by a
/// player alias (or nobody for system channels).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameChannel {
    /// Channel id (upstream exposes this number publicly).
    pub id: i64,
    /// Human-readable channel name.
    pub name: String,
    /// Owning alias; `null` for system-created channels (verified upstream).
    pub owner: Option<PlayerAlias>,
    /// Total messages ever sent in the channel.
    pub total_messages: i64,
    /// Current member count as reported by upstream.
    pub member_count: i64,
    /// Free-form props attached to the channel by the game.
    #[serde(default)]
    pub props: Vec<Prop>,
    /// Auto-delete the channel when the owner leaves or it empties.
    #[serde(default)]
    pub auto_cleanup: bool,
    /// Whether joining requires an invite (upstream JSON key `"private"`).
    #[serde(rename = "private", default)]
    pub is_private: bool,
    /// When the channel was created.
    pub created_at: DateTime<Utc>,
    /// When the channel was last updated.
    pub updated_at: DateTime<Utc>,
}

/// Why a player left a channel — the upstream TS numeric enum serialized as
/// an integer inside `v1.channels.player-left` (`meta.reason`).
///
/// Serde is implemented by hand because variant `rename` would produce JSON
/// *strings* (`"0"`), while upstream emits bare integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameChannelLeavingReason {
    /// Ordinary leave (disconnect or explicit leave).
    Default,
    /// Removed because the membership was temporary (member disconnected on
    /// a channel with `temporaryMembership` enabled).
    TemporaryMembership,
}

impl GameChannelLeavingReason {
    /// The upstream integer discriminant.
    fn as_i64(self) -> i64 {
        match self {
            Self::Default => 0,
            Self::TemporaryMembership => 1,
        }
    }
}

impl Serialize for GameChannelLeavingReason {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i64(self.as_i64())
    }
}

impl<'de> Deserialize<'de> for GameChannelLeavingReason {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = i64::deserialize(deserializer)?;
        match GameChannelLeavingReason::from_raw(raw) {
            Some(reason) => Ok(reason),
            None => Err(serde::de::Error::custom(format!(
                "unknown game-channel leaving reason: {raw}"
            ))),
        }
    }
}

impl GameChannelLeavingReason {
    /// Map an upstream integer discriminant back to a reason; `None` when
    /// unknown (upstream may add variants we do not model yet).
    fn from_raw(raw: i64) -> Option<Self> {
        match raw {
            0 => Some(Self::Default),
            1 => Some(Self::TemporaryMembership),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_channel() -> GameChannel {
        GameChannel {
            id: 1,
            name: "general-chat".into(),
            owner: None,
            total_messages: 308,
            member_count: 42,
            props: vec![Prop {
                key: "channelType".into(),
                value: "public".into(),
            }],
            auto_cleanup: false,
            is_private: false,
            created_at: "2024-12-09T12:00:00.000Z".parse().expect("parse ts"),
            updated_at: "2024-12-09T12:00:00.000Z".parse().expect("parse ts"),
        }
    }

    #[test]
    fn game_channel_serializes_camel_case_with_props() {
        let value = serde_json::to_value(sample_channel()).expect("serialize");
        let obj = value.as_object().expect("object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "autoCleanup",
                "createdAt",
                "id",
                "memberCount",
                "name",
                "owner",
                "private",
                "props",
                "totalMessages",
                "updatedAt"
            ]
        );
        // System channel: owner serializes as null (verified upstream shape).
        assert_eq!(value["owner"], serde_json::Value::Null);
        assert_eq!(obj["totalMessages"], 308);
        assert_eq!(obj["props"][0]["key"], "channelType");
        assert_eq!(obj["private"], false);
    }

    /// Fixture lifted verbatim from the game-channel API "list channels"
    /// sample response (docs.trytalo.com/docs/http/game-channel-api).
    #[test]
    fn game_channel_deserializes_from_upstream_fixture() {
        let fixture = serde_json::json!({
            "id": 2,
            "name": "guild-chat",
            "owner": {
                "id": 105,
                "service": "username",
                "identifier": "johnny_the_admin",
                "displayName": "johnny_the_admin",
                "player": {
                    "id": "85d67584-1346-4fad-a17f-fd7bd6c85364",
                    "props": [],
                    "devBuild": false,
                    "createdAt": "2024-10-25T18:18:28.000Z",
                    "lastSeenAt": "2024-12-04T07:15:13.000Z",
                    "groups": []
                },
                "lastSeenAt": "2024-12-04T07:15:13.000Z",
                "createdAt": "2024-10-25T18:18:28.000Z",
                "updatedAt": "2024-12-04T07:15:13.000Z"
            },
            "totalMessages": 36,
            "memberCount": 8,
            "props": [
                {"key": "channelType", "value": "guild"},
                {"key": "guildId", "value": "5912"}
            ],
            "autoCleanup": true,
            "private": false,
            "createdAt": "2024-12-09T12:00:00.000Z",
            "updatedAt": "2024-12-09T12:00:00.000Z"
        });

        let channel: GameChannel = serde_json::from_value(fixture).expect("deserialize");
        assert_eq!(channel.id, 2);
        assert_eq!(channel.name, "guild-chat");
        let owner = channel.owner.expect("owner present");
        assert_eq!(owner.identifier, "johnny_the_admin");
        assert_eq!(channel.total_messages, 36);
        assert_eq!(channel.member_count, 8);
        assert_eq!(channel.props.len(), 2);
        assert!(channel.auto_cleanup);
        assert!(!channel.is_private);
    }

    #[test]
    fn game_channel_leaves_props_absent_as_empty() {
        let fixture = serde_json::json!({
            "id": 1,
            "name": "general-chat",
            "owner": null,
            "totalMessages": 308,
            "memberCount": 42,
            "createdAt": "2024-12-09T12:00:00.000Z",
            "updatedAt": "2024-12-09T12:00:00.000Z"
        });

        let channel: GameChannel = serde_json::from_value(fixture).expect("deserialize");
        assert!(channel.props.is_empty());
        assert!(!channel.auto_cleanup);
        assert!(!channel.is_private);
    }

    #[test]
    fn leaving_reason_serializes_as_upstream_integers() {
        assert_eq!(
            serde_json::to_value(GameChannelLeavingReason::Default).expect("serialize"),
            serde_json::json!(0)
        );
        assert_eq!(
            serde_json::to_value(GameChannelLeavingReason::TemporaryMembership).expect("serialize"),
            serde_json::json!(1)
        );

        let back: GameChannelLeavingReason =
            serde_json::from_value(serde_json::json!(1)).expect("deserialize");
        assert_eq!(back, GameChannelLeavingReason::TemporaryMembership);

        let rejected: Result<GameChannelLeavingReason, _> =
            serde_json::from_value(serde_json::json!(7));
        assert!(rejected.is_err(), "unknown reason codes must be rejected");
    }
}
