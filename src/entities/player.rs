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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type)]
#[sqlx(type_name = "ENUM", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum PlayerStatus {
    Active,
    Suspended,
    Deleted,
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
