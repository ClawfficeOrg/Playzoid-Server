//! Feedback service — persists player feedback into the append-only
//! `analytics_events` table.
//!
//! All SQL touching feedback lives here so the HTTP layer
//! (`src/api/feedback.rs`) deals only in domain types. Unlike the events
//! service (fire-and-forget), feedback is **user content**: shape validation
//! happens synchronously *before* any SQL (invalid input → HTTP 400) and a
//! post-validation database failure propagates as
//! [`FeedbackServiceError::Database`] so the API can answer an honest 500 —
//! a fake success would silently drop the player's message.

use sqlx::MySqlPool;
use thiserror::Error;

/// Domain-level errors for feedback submission.
#[derive(Debug, Error)]
pub enum FeedbackServiceError {
    /// Request input failed pre-SQL validation (message blank/too long or
    /// encoded payload over the size cap). Nothing was written.
    #[error("invalid input: {0}")]
    Invalid(String),
    /// A database operation failed after validation passed. The feedback was
    /// **not** stored and must be reported to the client as a failure.
    #[error("database error")]
    Database(#[source] sqlx::Error),
}

/// Event key under which feedback rows are stored in `analytics_events`
/// (`props` carries `{ "message": ... }`). Rows are never read back by any
/// client-facing endpoint in v0.
pub const FEEDBACK_EVENT_NAME: &str = "feedback";

/// Maximum accepted feedback-message length (characters after trimming).
///
/// Mirrors the socket-chat gate precedent (task 0.3.13); bounds abuse until
/// rate limiting lands (task 0.4.8).
pub const MAX_MESSAGE_CHARS: usize = 1000;

/// Maximum serialized size (bytes) of the stored `props` JSON.
///
/// Mirrors the events/leaderboards `MAX_PROPS_BYTES` precedent; enforced
/// pre-SQL because MySQL JSON columns cannot be size-bounded in schema.
/// Only trips for escape-heavy messages (control chars expand ~6× under
/// JSON encoding) since plain-text 1000-char messages serialize well under
/// 4 KiB.
pub const MAX_PROPS_BYTES: usize = 4 * 1024;

/// One player-feedback submission as sent in a `POST /v1/feedback` body.
///
/// Unknown fields are rejected at deserialization time (validator precedent)
/// so malformed payloads surface as a clean 400 instead of silently dropping
/// data. There is deliberately no client-supplied timestamp or rating:
/// `created_at` is stamped by the database and richer schemas stay deferred
/// until a dedicated table exists (Phase 1.0 candidate).
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackInput {
    /// Free-form feedback text; trimmed before validation and storage.
    pub message: String,
}

/// Validate and persist one feedback submission.
///
/// Flow:
/// 1. Pre-SQL validation ([`validate`]) — blank/over-long messages or an
///    over-cap encoded payload reject with [`FeedbackServiceError::Invalid`]
///    before the database is touched.
/// 2. Best-effort attribution — same resolution rule as events/saves
///    (`status <> 'deleted'`); an unknown/deleted caller (or even a failing
///    resolution query) degrades to anonymous storage rather than an error:
///    account state must never lose feedback text.
/// 3. One parameterized `INSERT` into `analytics_events` with
///    `name = FEEDBACK_EVENT_NAME`. Any failure propagates as
///    [`FeedbackServiceError::Database`] — unlike analytics ingest this is
///    **not** swallowed: the caller must know their message was lost.
pub async fn submit_feedback(
    pool: &MySqlPool,
    player_public_id: &str,
    input: &FeedbackInput,
) -> Result<(), FeedbackServiceError> {
    let (_, props) = validate(input)?;

    // Best-effort attribution — events-service precedent: miss/failure →
    // NULL, never a hard error.
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
            tracing::warn!(error = ?e, "feedback: player attribution failed; storing anonymous");
            None
        }
    };

    // # SQL safety
    // Fully static statement, `.bind()`-only parameters — no user-controlled
    // string ever enters the SQL text.
    sqlx::query("INSERT INTO analytics_events (player_id, name, props) VALUES (?, ?, ?)")
        .bind(attributed_player_id)
        .bind(FEEDBACK_EVENT_NAME)
        .bind(&props)
        .execute(pool)
        .await
        .map_err(FeedbackServiceError::Database)?;
    Ok(())
}

/// Pure pre-SQL validation. Returns the trimmed message plus the ready-to-
/// store `props` payload (`{ "message": <trimmed> }`).
fn validate(input: &FeedbackInput) -> Result<(String, serde_json::Value), FeedbackServiceError> {
    let message = input.message.trim();
    if message.is_empty() || message.chars().count() > MAX_MESSAGE_CHARS {
        return Err(FeedbackServiceError::Invalid(format!(
            "message must be 1..={MAX_MESSAGE_CHARS} characters"
        )));
    }

    let props = serde_json::json!({ "message": message });
    if serialized_len(&props) > MAX_PROPS_BYTES {
        return Err(FeedbackServiceError::Invalid(
            "message exceeds the maximum encoded size".into(),
        ));
    }
    Ok((message.to_owned(), props))
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

    fn input(message: &str) -> FeedbackInput {
        FeedbackInput {
            message: message.into(),
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
            FeedbackServiceError::Invalid("bad message".into()).to_string(),
            "invalid input: bad message"
        );
        let db_err = FeedbackServiceError::Database(sqlx::Error::RowNotFound);
        assert_eq!(db_err.to_string(), "database error");
        assert!(
            std::error::Error::source(&db_err).is_some(),
            "Database variant must chain the underlying sqlx::Error"
        );
        assert!(
            std::error::Error::source(&FeedbackServiceError::Invalid("x".into())).is_none(),
            "Invalid variant carries a plain message"
        );
    }

    #[test]
    fn event_name_is_feedback() {
        assert_eq!(FEEDBACK_EVENT_NAME, "feedback");
    }

    #[test]
    fn validate_accepts_boundary_message_and_trims() {
        // Boundary: exactly MAX_MESSAGE_CHARS chars after trimming (raw body
        // padded past it must not trip the cap).
        let raw = format!(" {} ", "m".repeat(MAX_MESSAGE_CHARS));
        assert_eq!(raw.chars().count(), MAX_MESSAGE_CHARS + 2);

        let (message, props) = validate(&input(&raw)).expect("boundary message must validate");
        assert_eq!(message.chars().count(), MAX_MESSAGE_CHARS);
        assert!(message.starts_with('m'));
        assert!(message.ends_with('m'));
        assert_eq!(props["message"], serde_json::Value::String(message));
    }

    #[test]
    fn validate_rejects_empty_after_trim() {
        let result = validate(&input("   "));
        assert!(matches!(result, Err(FeedbackServiceError::Invalid(_))));
    }

    #[test]
    fn validate_rejects_over_1000_chars() {
        // Just past the cap must fail pre-SQL (fake pool never connects).
        let too_long = "m".repeat(MAX_MESSAGE_CHARS + 1);
        let result = validate(&input(&too_long));
        assert!(matches!(result, Err(FeedbackServiceError::Invalid(_))));
    }

    #[test]
    fn validate_rejects_oversized_encoded_props() {
        // Control chars expand ~6× under JSON encoding ("\u0001"), so a
        // length-valid message can still blow the 4 KiB encoded cap.
        let escape_heavy = "\u{1}".repeat(MAX_MESSAGE_CHARS);
        let result = validate(&input(&escape_heavy));
        assert!(matches!(result, Err(FeedbackServiceError::Invalid(_))));
    }

    #[tokio::test]
    async fn submit_rejects_blank_message_pre_sql() {
        // Validation runs before any SQL, so a dead pool is safe here.
        let pool = fake_pool();
        let result = submit_feedback(&pool, "player-uuid-1", &input("   ")).await;
        assert!(
            matches!(result, Err(FeedbackServiceError::Invalid(_))),
            "expected Invalid, got {result:?}"
        );
    }
}
