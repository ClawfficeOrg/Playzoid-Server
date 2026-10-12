//! Events service — sqlx-backed ingestion for the `analytics_events` table.
//!
//! All SQL touching analytics events lives here so the HTTP layer
//! (`src/api/events.rs`) deals only in domain types. The contract is
//! **fire-and-forget**: shape validation happens synchronously *before* any
//! SQL (a whole batch is rejected → HTTP 400), while database-level failures
//! are logged and swallowed at the API layer — telemetry loss must never
//! block or fail a client response.

use crate::entities::analytics_event::AnalyticsEventRow;
use sqlx::{MySqlPool, QueryBuilder};
use thiserror::Error;

/// Domain-level errors for analytics-event ingestion.
#[derive(Debug, Error)]
pub enum EventsServiceError {
    /// Request input failed pre-SQL validation (batch size/count, event
    /// names, props size). Nothing was written.
    #[error("invalid input: {0}")]
    Invalid(String),
    /// A database operation failed after validation passed.
    #[error("database error")]
    Database(#[source] sqlx::Error),
}

/// Maximum number of events accepted in one batch.
///
/// Bounds the worst-case per-request cost (~400 KiB of props plus 100
/// placeholder groups, far below MySQL's parameter limit) until rate
/// limiting lands (task 0.4.8).
pub const MAX_BATCH_EVENTS: usize = 100;

/// Maximum length of a single event's `name` — matches `VARCHAR(64)`.
pub const MAX_EVENT_NAME_LEN: usize = 64;

/// Maximum serialized size (bytes) accepted for one event's `props` JSON.
///
/// Mirrors the leaderboards' `MAX_PROPS_BYTES` precedent; enforced pre-SQL
/// because MySQL JSON columns cannot be size-bounded in schema.
pub const MAX_PROPS_BYTES: usize = 4 * 1024;

/// One analytics event as submitted in a batched `POST /v1/events` body.
///
/// Unknown fields are rejected at deserialization time (validator precedent)
/// so malformed telemetry surfaces as a clean 400 instead of silently
/// dropping data. There is deliberately no client-supplied timestamp:
/// `created_at` is stamped by the database, keeping the log append-only.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventInput {
    /// Free-form event key (typed schema deferred to Phase 1.0).
    pub name: String,
    /// Arbitrary JSON payload; omitted or `null` both store SQL `NULL`.
    pub props: Option<serde_json::Value>,
}

/// Ingest a batch of analytics events and return the number accepted.
///
/// Flow:
/// 1. Pre-SQL validation ([`validate_batch`]) — any invalid event rejects
///    the *whole* batch atomically before the database is touched.
/// 2. Best-effort attribution — the caller's JWT `public_id` resolves to the
///    internal `players.id`; an unknown or deleted player (or even a failing
///    resolution query) degrades to anonymous rows rather than an error:
///    account state must never break telemetry intake.
/// 3. One batched multi-row `INSERT`. On success the number of events is
///    returned; a database failure propagates and the API layer logs it but
///    still answers 202 (fire-and-forget).
pub async fn ingest_events(
    pool: &MySqlPool,
    player_public_id: &str,
    events: &[EventInput],
) -> Result<usize, EventsServiceError> {
    let rows = validate_batch(events)?;

    // Best-effort attribution — same resolution rule as the saves service
    // (`status <> 'deleted'`), but a miss stores NULL instead of a NotFound,
    // and even a query failure only degrades to anonymous rows.
    let attributed_player_id: Option<u64> = match sqlx::query_as::<_, (u64,)>(
        "SELECT id FROM players WHERE public_id = ? AND status <> 'deleted'",
    )
    .bind(player_public_id)
    .fetch_optional(pool)
    .await
    {
        Ok(Some((id,))) => Some(id),
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(error = ?e, "events: player attribution failed; storing anonymous");
            None
        }
    };

    let rows: Vec<AnalyticsEventRow> = rows
        .into_iter()
        .map(|row| AnalyticsEventRow {
            player_id: attributed_player_id,
            ..row
        })
        .collect();

    insert_rows(pool, &rows).await?;
    Ok(rows.len())
}

/// Validate a submitted batch entirely before any SQL runs.
///
/// Rules: non-empty, ≤ [`MAX_BATCH_EVENTS`] events, each `name` trims to
/// 1..=[`MAX_EVENT_NAME_LEN`] chars, and each present `props` serializes
/// within [`MAX_PROPS_BYTES`]. Returns cleaned insert rows; `player_id` is
/// attached later by [`ingest_events`].
fn validate_batch(events: &[EventInput]) -> Result<Vec<AnalyticsEventRow>, EventsServiceError> {
    if events.is_empty() {
        return Err(EventsServiceError::Invalid(
            "batch must contain at least one event".into(),
        ));
    }
    if events.len() > MAX_BATCH_EVENTS {
        return Err(EventsServiceError::Invalid(format!(
            "batch exceeds the maximum of {MAX_BATCH_EVENTS} events"
        )));
    }

    let mut rows = Vec::with_capacity(events.len());
    for (idx, event) in events.iter().enumerate() {
        let name = event.name.trim();
        if name.is_empty() || name.chars().count() > MAX_EVENT_NAME_LEN {
            return Err(EventsServiceError::Invalid(format!(
                "event[{idx}] name must be 1..={MAX_EVENT_NAME_LEN} characters"
            )));
        }
        if let Some(props) = &event.props
            && serialized_len(props) > MAX_PROPS_BYTES
        {
            return Err(EventsServiceError::Invalid(format!(
                "event[{idx}] props exceed the maximum size"
            )));
        }
        rows.push(AnalyticsEventRow {
            player_id: None,
            name: name.to_owned(),
            props: event.props.clone(),
        });
    }
    Ok(rows)
}

/// Insert all rows in a single multi-row statement.
///
/// # SQL safety
/// Only `(?, ?, ?)` placeholder scaffolding is appended to the statement —
/// the repetition count comes from the already-validated in-memory batch
/// length, and every value is attached through `push_bind`. No
/// user-controlled string ever enters the SQL text, giving exactly the same
/// injection guarantee as the static `.bind()`ed queries elsewhere in this
/// codebase. 100 groups × 3 parameters stays far below MySQL's
/// prepared-statement parameter limit.
async fn insert_rows(
    pool: &MySqlPool,
    rows: &[AnalyticsEventRow],
) -> Result<(), EventsServiceError> {
    let mut builder = QueryBuilder::new("INSERT INTO analytics_events (player_id, name, props) ");
    builder.push_values(rows.iter(), |mut b, row| {
        b.push_bind(row.player_id)
            .push_bind(&row.name)
            .push_bind(&row.props);
    });
    builder
        .build()
        .execute(pool)
        .await
        .map_err(EventsServiceError::Database)?;
    Ok(())
}

/// Serialized byte length of a JSON value, saturating if it cannot serialize.
fn serialized_len(value: &serde_json::Value) -> usize {
    serde_json::to_string(value)
        .map(|s| s.len())
        .unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(name: &str, props: Option<serde_json::Value>) -> EventInput {
        EventInput {
            name: name.into(),
            props,
        }
    }

    /// Lazy pool that never connects — safe for pre-SQL validation paths.
    fn fake_pool() -> MySqlPool {
        MySqlPool::connect_lazy("mysql://test:test@127.0.0.1/test")
            .expect("lazy pool creation should not fail")
    }

    #[test]
    fn error_display_and_source_chain() {
        assert_eq!(
            EventsServiceError::Invalid("bad batch".into()).to_string(),
            "invalid input: bad batch"
        );
        let db_err = EventsServiceError::Database(sqlx::Error::RowNotFound);
        assert_eq!(db_err.to_string(), "database error");
        assert!(
            std::error::Error::source(&db_err).is_some(),
            "Database variant must chain the underlying sqlx::Error"
        );
        assert!(
            std::error::Error::source(&EventsServiceError::Invalid("x".into())).is_none(),
            "Invalid variant carries a plain message"
        );
    }

    #[tokio::test]
    async fn ingest_rejects_empty_batch_pre_sql() {
        let pool = fake_pool();
        let result = ingest_events(&pool, "player-uuid-1", &[]).await;
        assert!(
            matches!(result, Err(EventsServiceError::Invalid(_))),
            "expected Invalid, got {result:?}"
        );
    }

    #[test]
    fn validate_rejects_over_max_batch() {
        let batch: Vec<EventInput> = (0..=MAX_BATCH_EVENTS)
            .map(|i| input(&format!("evt-{i}"), None))
            .collect();
        let result = validate_batch(&batch);
        assert!(matches!(result, Err(EventsServiceError::Invalid(_))));
    }

    #[test]
    fn validate_rejects_blank_name() {
        let batch = vec![input("   ", None)];
        let result = validate_batch(&batch);
        assert!(matches!(result, Err(EventsServiceError::Invalid(_))));
    }

    #[test]
    fn validate_rejects_over_64_char_name() {
        // Boundary: exactly 64 chars passes; just past it must fail pre-SQL.
        let batch = vec![input(&"n".repeat(MAX_EVENT_NAME_LEN + 1), None)];
        let result = validate_batch(&batch);
        assert!(matches!(result, Err(EventsServiceError::Invalid(_))));
    }

    #[test]
    fn validate_rejects_oversized_props() {
        // One string over the byte cap forces the size check to trip before
        // any SQL runs (fake pool never connects).
        let big = serde_json::json!({ "data": "x".repeat(MAX_PROPS_BYTES) });
        let batch = vec![input("evt", Some(big))];
        let result = validate_batch(&batch);
        assert!(matches!(result, Err(EventsServiceError::Invalid(_))));
    }

    #[test]
    fn validate_accepts_boundaries_and_trims_names() {
        // Exactly-at-limit values must pass: 100 events, names trimming to
        // exactly 64 chars, props serializing to exactly MAX_PROPS_BYTES.
        let boundary_props = serde_json::json!({ "d": "x".repeat(MAX_PROPS_BYTES - 8) });
        assert_eq!(serialized_len(&boundary_props), MAX_PROPS_BYTES);

        let padded_name = format!(" {} ", "n".repeat(MAX_EVENT_NAME_LEN));
        let batch: Vec<EventInput> = (0..MAX_BATCH_EVENTS)
            .map(|_| input(&padded_name, Some(boundary_props.clone())))
            .collect();
        let rows = validate_batch(&batch).expect("boundary batch must validate");

        assert_eq!(rows.len(), MAX_BATCH_EVENTS);
        assert_eq!(rows[0].name.chars().count(), MAX_EVENT_NAME_LEN);
        assert!(rows[0].name.starts_with('n'));
        assert!(rows[0].name.ends_with('n'));
        assert_eq!(rows[0].player_id, None, "attribution happens later");
        assert_eq!(rows[MAX_BATCH_EVENTS - 1].props, Some(boundary_props));
    }
}
