//! Redis connection helpers.
//!
//! Centralises how the process establishes its Redis [`ConnectionManager`] so
//! every call site gets identical, **bounded** startup behaviour.
//!
//! ## Why this module exists
//!
//! `redis::aio::ConnectionManager::new` has no connect timeout by default and
//! its internal retry loop re-arms indefinitely when the server is unreachable
//! (the observed failure: the process never reached `HttpServer::bind`, so
//! "degraded mode" never degraded — see `docs/learnings.md`, 2026-10-12). Any
//! caller that treats Redis as optional must therefore wrap the handshake in
//! its own timeout and fall back to `None` when it trips, matching the
//! degraded-mode contract used for the MySQL pool.

use std::time::Duration;

/// How long to allow the initial Redis handshake before giving up and running
/// degraded. Deliberately short: Redis backs optional caching and rate
/// limiting, so a slow/unreachable Redis must not delay startup or liveness
/// probes.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Connect to Redis with a bounded handshake.
///
/// Returns `Ok(Some(manager))` on success, and `Err` with a human-readable
/// cause when the URL is invalid, the handshake fails, or it exceeds
/// [`CONNECT_TIMEOUT`]. Callers are expected to warn-log and continue in
/// degraded mode on `Err`, rather than aborting startup.
pub async fn connect(redis_url: &str) -> Result<redis::aio::ConnectionManager, String> {
    let client = redis::Client::open(redis_url).map_err(|e| format!("invalid Redis URL: {e}"))?;

    match tokio::time::timeout(CONNECT_TIMEOUT, redis::aio::ConnectionManager::new(client)).await {
        Ok(Ok(mgr)) => Ok(mgr),
        Ok(Err(e)) => Err(format!("Redis connection failed: {e}")),
        Err(_) => Err(format!(
            "Redis connection timed out after {}s",
            CONNECT_TIMEOUT.as_secs()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_url_is_rejected_without_connecting() {
        // `ConnectionManager` is not `Debug`, so unwrap the error by hand.
        let err = match tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(connect("not-a-valid-url"))
        {
            Ok(_) => panic!("bad URL must fail"),
            Err(e) => e,
        };
        assert!(err.contains("invalid Redis URL"), "got: {err}");
    }

    #[test]
    fn unreachable_server_times_out_within_budget() {
        // Port 0 is never a listening Redis; the handshake must give up
        // within the budget instead of retrying forever.
        let started = std::time::Instant::now();
        let err = match tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(connect("redis://127.0.0.1:0"))
        {
            Ok(_) => panic!("unreachable Redis must fail"),
            Err(e) => e,
        };
        let elapsed = started.elapsed();

        assert!(
            err.contains("timed out") || err.contains("connection failed"),
            "unexpected error: {err}"
        );
        assert!(
            elapsed < CONNECT_TIMEOUT + Duration::from_secs(2),
            "handshake took {elapsed:?}, expected bounded by {CONNECT_TIMEOUT:?}"
        );
    }
}
