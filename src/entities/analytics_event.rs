//! `AnalyticsEventRow` — insert-shape domain struct for the `analytics_events`
//! table.
//!
//! See `migrations/20260826000002_create_analytics_events.up.sql` for the
//! canonical schema. Unlike every other entity here there is deliberately
//! **no `FromRow` row type and no public `View` projection**: the table is
//! append-only and write-only in v0 (task 0.4.6 fire-and-forget ingest) —
//! rows are never selected, updated, or addressed by clients, so nothing
//! exists to read back and project.

/// One validated analytics event ready for insertion into `analytics_events`.
///
/// Produced by the events service's pre-SQL validation; `player_id` starts
/// as [`None`] there and is overwritten once per request with best-effort
/// attribution of the caller's JWT identity (anonymous events stay `NULL`,
/// matching the schema's nullable FK).
#[derive(Debug, Clone)]
pub(crate) struct AnalyticsEventRow {
    /// Internal `players.id` of the emitting player, or [`None`] when the
    /// event is anonymous (unresolved or deleted caller — never a hard
    /// failure).
    pub(crate) player_id: Option<u64>,
    /// Validated, trimmed event name (1..=64 chars, matches `VARCHAR(64)`).
    pub(crate) name: String,
    /// Optional JSON payload (serialized size capped at the API layer).
    pub(crate) props: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_carries_optional_fields_unchanged() {
        let anonymous = AnalyticsEventRow {
            player_id: None,
            name: "level_complete".into(),
            props: None,
        };
        assert!(anonymous.player_id.is_none());
        assert!(anonymous.props.is_none());

        let attributed = AnalyticsEventRow {
            player_id: Some(42),
            name: "iap_purchase".into(),
            props: Some(serde_json::json!({ "sku": "coins-100" })),
        };
        assert_eq!(attributed.player_id, Some(42));
        assert_eq!(
            attributed.props,
            Some(serde_json::json!({ "sku": "coins-100" }))
        );
    }

    #[test]
    fn row_name_round_trips() {
        let name = "session_start";
        let row = AnalyticsEventRow {
            player_id: None,
            name: name.into(),
            props: None,
        };
        assert_eq!(row.name, name);
    }
}
