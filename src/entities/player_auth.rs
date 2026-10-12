//! `PlayerAuth` domain model — the optional `auth` block nested inside the
//! upstream `Player` object (see the Talo socket reference and player-auth
//! API; mapped in `docs/TALO_API.md` → domain models).
//!
//! Security invariant: this type is wire-facing and must never carry
//! credential material — Playzoid keeps `password_hash` on the internal
//! `players` row only (`src/entities/player.rs`). A unit test below pins
//! that no `password` key can appear in a serialized `PlayerAuth`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Upstream `Player.auth?`: email-verification state for a player account.
///
/// Present only for players registered through Talo's auth service; absent
/// (Rust `None`) for anonymous/service aliases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerAuth {
    /// Verified email address, when one is on file.
    pub email: Option<String>,
    /// Whether the upstream game requires email verification for this player.
    #[serde(default)]
    pub verification_enabled: bool,
    /// When the current session was created, if one is active.
    #[serde(default)]
    pub session_created_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PlayerAuth {
        PlayerAuth {
            email: Some("alice@example.com".into()),
            verification_enabled: true,
            session_created_at: Some("2026-08-25T12:00:00Z".parse().expect("parse ts")),
        }
    }

    #[test]
    fn player_auth_serializes_camel_case() {
        let value = serde_json::to_value(sample()).expect("serialize");
        let obj = value.as_object().expect("object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["email", "sessionCreatedAt", "verificationEnabled"]
        );
        assert_eq!(obj["verificationEnabled"], true);
    }

    #[test]
    fn player_auth_never_exposes_password() {
        let serialized = serde_json::to_string(&sample()).expect("serialize");
        assert!(
            !serialized.to_lowercase().contains("password"),
            "PlayerAuth must never carry credential material: {serialized}"
        );
    }

    #[test]
    fn player_auth_email_and_session_optional() {
        let auth = PlayerAuth {
            email: None,
            verification_enabled: false,
            session_created_at: None,
        };

        let value = serde_json::to_value(&auth).expect("serialize");
        assert_eq!(value["email"], serde_json::Value::Null);
        assert_eq!(value["sessionCreatedAt"], serde_json::Value::Null);

        // Missing keys deserialize to defaults (upstream omits them for
        // unverified players).
        let back: PlayerAuth =
            serde_json::from_value(serde_json::json!({"verificationEnabled": false}))
                .expect("deserialize");
        assert_eq!(back.email, None);
        assert_eq!(back.session_created_at, None);
    }
}
