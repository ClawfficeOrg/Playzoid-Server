//! Database connection pool wiring.
//!
//! Provides a single helper, [`build_pool`], which constructs the application's
//! MySQL connection pool. Configuration arrives via the typed [`crate::config::Config`]
//! struct populated in `main`.

use sqlx::mysql::{MySqlPool, MySqlPoolOptions};
use std::time::Duration;

/// Build a MySQL connection pool from a database URL.
///
/// Pool tuning:
/// - `max_connections = 10` — sane default for a single application instance.
/// - `acquire_timeout = 5s` — fail fast under sustained pressure rather than
///   stacking request latency.
///
/// The function is `async` because [`MySqlPoolOptions::connect`] performs a
/// handshake against the database; it returns immediately once the pool is
/// initialised (subsequent connections are lazy on demand).
pub async fn build_pool(url: &str) -> sqlx::Result<MySqlPool> {
    MySqlPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(url)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: only runs when `MYSQL_URL` is set in the environment.
    /// Provides an opt-in integration check that the pool can hand out a
    /// connection capable of executing a trivial query.
    #[tokio::test]
    #[ignore = "requires a live MySQL; set MYSQL_URL to enable"]
    async fn pool_can_select_one() {
        let url = std::env::var("MYSQL_URL").expect("MYSQL_URL must be set for this test");
        let pool = build_pool(&url).await.expect("pool builds");
        let row: (i64,) = sqlx::query_as("SELECT 1")
            .fetch_one(&pool)
            .await
            .expect("select 1");
        assert_eq!(row.0, 1);
    }
}
