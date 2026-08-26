//! Shared `Prop` domain model — the upstream key/value pair reused across
//! Talo entities (`Player.props`, `GameChannel.props`,
//! `LeaderboardEntry.props`, live config). Verified against the Talo socket
//! reference type `Prop = { key: string, value: string }` and the game-channel
//! API samples (see `docs/TALO_API.md` → domain models).

use serde::{Deserialize, Serialize};

/// A single upstream `Prop`: an opaque string key paired with a string value.
///
/// Both fields are plain strings upstream (numeric or structured data is
/// stringified by clients); keep this shape byte-compatible with Talo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prop {
    /// Prop key (unique within its owning collection).
    pub key: String,
    /// Prop value; always a string on the wire.
    pub value: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prop_serializes_key_value_roundtrip() {
        let prop = Prop {
            key: "xPos".into(),
            value: "13.29".into(),
        };

        let value = serde_json::to_value(&prop).expect("serialize");
        assert_eq!(value, serde_json::json!({"key": "xPos", "value": "13.29"}));

        let back: Prop = serde_json::from_value(value).expect("deserialize");
        assert_eq!(back, prop);
    }
}
