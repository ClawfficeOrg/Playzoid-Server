//! Redis-backed player session cache.
//!
//! Provides lightweight get/set/invalidate helpers for [`crate::entities::player::PlayerView`]
//! objects keyed by `public_id`. All operations are best-effort — callers
//! should log and continue on [`CacheError`] rather than surfacing it to the
//! client.
//!
//! Key format: `player:<public_id>` (no namespace collision risk within a
//! single Playzoid Redis instance; add a prefix if multi-tenant Redis is
//! ever introduced).

use redis::{AsyncCommands, aio::ConnectionManager};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors from cache operations.
#[derive(Debug, Error)]
pub enum CacheError {
    #[error("redis error: {0}")]
    Redis(#[from] redis::RedisError),
    #[error("serialization error: {0}")]
    Serialize(#[from] serde_json::Error),
}

fn player_key(public_id: &str) -> String {
    format!("player:{public_id}")
}

/// Retrieve a cached value for `public_id`, deserializing from JSON.
///
/// Returns `Ok(None)` on a cache miss or if Redis is unreachable (callers
/// should treat `Err` as a miss and fall back to the database).
pub async fn get_player_view<T>(
    mut conn: ConnectionManager,
    public_id: &str,
) -> Result<Option<T>, CacheError>
where
    T: for<'de> Deserialize<'de>,
{
    let key = player_key(public_id);
    let raw: Option<String> = conn.get(&key).await?;
    raw.map(|s| serde_json::from_str::<T>(&s))
        .transpose()
        .map_err(CacheError::Serialize)
}

/// Store `value` for `public_id` with the given TTL.
///
/// A `ttl_secs` of `0` is clamped to `3600` to avoid storing entries with no
/// expiry.
pub async fn set_player_view<T>(
    mut conn: ConnectionManager,
    public_id: &str,
    value: &T,
    ttl_secs: u64,
) -> Result<(), CacheError>
where
    T: Serialize,
{
    let key = player_key(public_id);
    let json = serde_json::to_string(value)?;
    let ttl = if ttl_secs == 0 { 3600 } else { ttl_secs };
    let _: () = conn.set_ex(&key, json, ttl).await?;
    Ok(())
}

/// Remove a cached player entry. Called on profile update or soft-delete.
pub async fn invalidate_player(
    mut conn: ConnectionManager,
    public_id: &str,
) -> Result<(), CacheError> {
    let key = player_key(public_id);
    let _: () = conn.del(&key).await?;
    Ok(())
}
