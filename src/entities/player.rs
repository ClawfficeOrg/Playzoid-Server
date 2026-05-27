//! `Player` entity — row representation of the `players` table.
//!
//! See `migrations/20260501000001_create_players.up.sql` for the canonical
//! schema and `docs/TALO_API.md` for the public API mapping. The internal
//! `id` (BIGINT) is *never* exposed externally; APIs always project
//! `public_id` (UUID) instead.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;

/// Lifecycle state for a player row. Mapped to the MySQL ENUM column.
///
/// `Deleted` is a tombstone — the row stays for FK integrity until purged
/// by a future maintenance job.
///
/// We implement `Type` and `Decode` manually because MySQL returns ENUM
/// columns as text at the wire level; the derive macro's `type_name = "ENUM"`
/// hint does not survive sqlx's type-compatibility check on MySQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayerStatus {
    Active,
    Suspended,
    Deleted,
}

use sqlx::TypeInfo as _; // needed for .name() on MySqlTypeInfo

// Treat PlayerStatus as a MySQL string so sqlx decodes ENUM columns correctly.
impl sqlx::Type<sqlx::MySql> for PlayerStatus {
    fn type_info() -> sqlx::mysql::MySqlTypeInfo {
        <String as sqlx::Type<sqlx::MySql>>::type_info()
    }
    fn compatible(ty: &sqlx::mysql::MySqlTypeInfo) -> bool {
        <String as sqlx::Type<sqlx::MySql>>::compatible(ty)
            || ty.name().eq_ignore_ascii_case("ENUM")
    }
}

impl<'r> sqlx::Decode<'r, sqlx::MySql> for PlayerStatus {
    fn decode(
        value: sqlx::mysql::MySqlValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let s = <String as sqlx::Decode<'r, sqlx::MySql>>::decode(value)?;
        match s.as_str() {
            "active" => Ok(PlayerStatus::Active),
            "suspended" => Ok(PlayerStatus::Suspended),
            "deleted" => Ok(PlayerStatus::Deleted),
            other => Err(format!("unknown player status: {other:?}").into()),
        }
    }
}

/// Full player row, mirroring the `players` table 1:1.
#[derive(Debug, Clone, FromRow)]
#[allow(dead_code)] // Fields are consumed incrementally by Phase 0.2 services.
pub struct Player {
    pub id: u64,
    pub public_id: String,
    pub username: String,
    pub email: Option<String>,
    pub password_hash: String,
    pub parent_account_id: Option<u64>,
    pub status: PlayerStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

/// Public projection of a `Player` — the shape returned by HTTP endpoints.
///
/// Critically omits `id` and `password_hash`; surfaces `public_id` as `id`
/// to external consumers.
#[derive(Debug, Clone, Serialize)]
pub struct PlayerView {
    pub id: String,
    pub username: String,
    pub email: Option<String>,
    pub parent_account_id: Option<String>,
    pub status: PlayerStatus,
    pub created_at: DateTime<Utc>,
}

impl From<&Player> for PlayerView {
    fn from(p: &Player) -> Self {
        // For the parent we only have the BIGINT FK on this row; resolving it
        // to the parent's public_id requires a join and is the caller's job.
        // We surface `None` here when not joined; `Some(parent_public_id)` is
        // set by callers that did the join.
        Self {
            id: p.public_id.clone(),
            username: p.username.clone(),
            email: p.email.clone(),
            parent_account_id: None,
            status: p.status,
            created_at: p.created_at,
        }
    }
}
