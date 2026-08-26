//! Redis-backed fixed-window rate limiting for public routes (task 0.4.8).
//!
//! The [`RateLimit`] middleware is registered globally but only enforces on
//! configured public path prefixes (default: `/v1/auth`, `/auth`, `/ws`).
//! Two request classes exist:
//!
//! - **auth** — credential endpoints (`/v1/auth/**`, legacy `/auth/**`),
//!   with a tight budget to slow brute-force attempts.
//! - **default** — every other rate-limited public route (e.g. the `/ws`
//!   handshake).
//!
//! Counters live in Redis as one key per `(class, client ip, window start)`
//! (`rl:{class}:{ip}:{window_start}`), incremented by an atomic `EVAL`
//! script, so limits hold across worker threads and (once multi-node lands)
//! server instances. Buckets are wall-clock aligned fixed windows.
//!
//! # Degraded mode
//!
//! Fail-open is deliberate and matches the repo-wide degraded-mode pattern:
//! if Redis errors mid-flight the request proceeds (warn-logged), and when no
//! [`RateLimiter`] app data is registered (Redis down at boot, or disabled)
//! the middleware passes everything through. Availability beats strictness
//! for v0; see `docs/memory.md`.
//!
//! # Client identification
//!
//! Requests are keyed by socket peer IP. `X-Forwarded-For` trust is opt-in
//! via `RATE_LIMIT_TRUST_XFF=true` because the header is trivially spoofable
//! without an overwriting proxy. Requests with no identifiable peer address
//! pass through rather than sharing one unkeyed bucket.
//!
//! # Example
//!
//! ```rust,ignore
//! use playzoid_server::middleware::rate_limit::{RateLimiter, RateLimit};
//!
//! App::new()
//!     .wrap(RateLimit)
//!     .app_data(web::Data::new(RateLimiter::new(redis_mgr, cfg.rate_limit)))
//! ```

use crate::config::RateLimitConfig;
use actix_web::{
    Error, HttpResponse,
    body::MessageBody,
    dev::{Service, ServiceRequest, ServiceResponse, Transform, forward_ready},
    http::header,
    web,
};
use redis::aio::ConnectionManager;
use serde_json::json;
use std::{
    future::{Future, Ready, ready},
    pin::Pin,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

/// Lua script implementing the atomic fixed-window counter.
///
/// `KEYS[1]` is the bucket key, `ARGV[1]` the window TTL in seconds.
/// Returns `(count, ttl_secs)` where `ttl_secs` is how long until the
/// bucket expires (the time remaining in this window).
const FIXED_WINDOW_LUA: &str = r#"
local count = redis.call('INCR', KEYS[1])
if count == 1 then
  redis.call('EXPIRE', KEYS[1], ARGV[1])
end
local ttl = redis.call('TTL', KEYS[1])
return {count, ttl}
"#;

const HEADER_LIMIT: &str = "x-ratelimit-limit";
const HEADER_REMAINING: &str = "x-ratelimit-remaining";
const HEADER_RESET: &str = "x-ratelimit-reset";

/// Errors raised while consulting the rate-limit backend.
#[derive(Debug, thiserror::Error)]
pub enum RateLimitError {
    /// The Redis operation failed. Middleware treats this as fail-open.
    #[error("redis error: {0}")]
    Redis(#[from] redis::RedisError),
}

/// Request classification used to pick the applicable budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitClass {
    /// Credential endpoints (`/v1/auth/**`, `/auth/**`) — tightest budget.
    Auth,
    /// Every other rate-limited public route — general budget.
    Default,
}

impl RateLimitClass {
    /// Bucket-key label for this class.
    fn label(self) -> &'static str {
        match self {
            RateLimitClass::Auth => "auth",
            RateLimitClass::Default => "default",
        }
    }

    /// `(limit, window_secs)` pair configured for this class.
    fn budget(self, config: &RateLimitConfig) -> (u32, u64) {
        match self {
            RateLimitClass::Auth => (config.auth_requests, config.auth_window_secs),
            RateLimitClass::Default => (config.requests, config.window_secs),
        }
    }
}

/// Path prefixes that always classify as [`RateLimitClass::Auth`], regardless
/// of what `RATE_LIMIT_PUBLIC_PREFIXES` contains. Both the canonical upstream
/// spelling and the 0.4.1 legacy alias are listed.
const AUTH_PREFIXES: &[&str] = &["/v1/auth", "/auth"];

/// Outcome of classifying a request path against the limiter configuration.
///
/// Returns `None` for paths outside every configured prefix — such requests
/// bypass rate limiting entirely (`/healthz` must never be listed).
pub fn classify_path(path: &str, config: &RateLimitConfig) -> Option<RateLimitClass> {
    if AUTH_PREFIXES.iter().any(|p| path_matches(path, p)) {
        return Some(RateLimitClass::Auth);
    }
    if config.public_prefixes.iter().any(|p| path_matches(path, p)) {
        return Some(RateLimitClass::Default);
    }
    None
}

/// Exact-prefix or segment-boundary match: `/v1/auth` matches `/v1/auth` and
/// `/v1/auth/login` but never `/v1/authentication`.
fn path_matches(path: &str, prefix: &str) -> bool {
    path == prefix || path.starts_with(&format!("{prefix}/"))
}

/// Resolve the client identifier for bucket keys.
///
/// Preference order: first hop of `X-Forwarded-For` (only when `trust_xff`),
/// else the socket peer address. Returns `None` when neither yields anything
/// usable — callers must treat that as pass-through.
pub fn extract_client_ip(
    peer_addr: Option<&str>,
    xff_header: Option<&str>,
    trust_xff: bool,
) -> Option<String> {
    if trust_xff && let Some(first_hop) = xff_header.and_then(|v| v.split(',').next()) {
        let candidate = first_hop.trim();
        if !candidate.is_empty() {
            return Some(candidate.to_string());
        }
    }
    peer_addr.filter(|p| !p.is_empty()).map(str::to_string)
}

/// Build the deterministic bucket key for one window slice.
///
/// Format: `rl:{class}:{ip}:{window_start}` where `window_start` is the epoch
/// second the current fixed window began.
fn bucket_key(class: RateLimitClass, client_ip: &str, window_start: u64) -> String {
    format!("rl:{}:{}:{}", class.label(), client_ip, window_start)
}

/// Result of one counter hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitDecision {
    /// Whether the request may proceed.
    pub allowed: bool,
    /// Configured limit for the request's class.
    pub limit: u64,
    /// Requests still available in this window (never below zero).
    pub remaining: u64,
    /// Seconds until the window resets; `0` while under limit. Minimum 1
    /// when blocked so `Retry-After` never advertises zero delay.
    pub retry_after_secs: u64,
    /// Epoch second at which the window resets (for `X-RateLimit-Reset`).
    pub reset_epoch_secs: u64,
}

/// Pure decision math shared by the Redis-backed check and unit tests.
///
/// `count` is the post-increment usage of the window, `ttl_secs` the backend
/// TTL (seconds until the window ends; non-positive values floor the blocked
/// `Retry-After` at 1), `now_secs` the current epoch second.
pub fn decision_from_count(
    limit: u32,
    count: i64,
    ttl_secs: i64,
    now_secs: u64,
) -> RateLimitDecision {
    let allowed = count <= i64::from(limit);
    let used = count.max(0) as u64;
    let remaining = u64::from(limit).saturating_sub(used);
    // Clamp keeps a nonsense/negative backend TTL from advertising zero or
    // absurd delay; the ceiling mirrors the config-side window cap.
    let retry_after_secs = if allowed {
        0
    } else {
        ttl_secs.clamp(1, 3600) as u64
    };
    let reset_epoch_secs = now_secs + retry_after_secs.max(ttl_secs.max(0) as u64);
    RateLimitDecision {
        allowed,
        limit: u64::from(limit),
        remaining,
        retry_after_secs,
        reset_epoch_secs,
    }
}

/// Boxed `'static` future a [`WindowCounter`] hit returns.
type CounterFuture = Pin<Box<dyn Future<Output = Result<(i64, i64), RateLimitError>> + Send>>;

/// Abstract fixed-window counter so the middleware can be unit-tested
/// without a live Redis.
///
/// Implementations return `(count, ttl_secs)` after atomically incrementing
/// `key`, creating it with a `window_secs` expiry on the first hit.
///
/// Object-safe so the limiter can store `Arc<dyn WindowCounter>` and stay a
/// single concrete type for `app_data` lookups.
pub trait WindowCounter: Send + Sync + 'static {
    /// Atomically increment `key` and report `(count, ttl_secs)`.
    ///
    /// Returns a `'static` future — implementations copy everything they need
    /// (connection, key, hits) into the future up front.
    fn hit(&self, key: &str, window_secs: u64) -> CounterFuture;
}

/// Redis-backed [`WindowCounter`] driven by the atomic `EVAL` script.
#[derive(Clone)]
pub struct RedisWindowCounter {
    conn: ConnectionManager,
}

impl WindowCounter for RedisWindowCounter {
    #[tracing::instrument(skip(self))]
    fn hit(&self, key: &str, window_secs: u64) -> CounterFuture {
        let conn = self.conn.clone();
        let key = key.to_string();
        Box::pin(async move {
            // Clones share one multiplexed connection and transparently
            // reconnect, matching the src/services/cache.rs usage pattern.
            let mut conn = conn;
            let result: (i64, i64) = redis::cmd("EVAL")
                .arg(FIXED_WINDOW_LUA)
                .arg(1)
                .arg(key)
                .arg(window_secs)
                .query_async(&mut conn)
                .await?;
            Ok(result)
        })
    }
}

/// Source of "now" as a unix epoch second — injectable so tests can advance
/// wall time and observe fixed-window rollover.
type NowFn = Arc<dyn Fn() -> u64 + Send + Sync>;

/// System-clock [`NowFn`].
fn system_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// Shared rate-limiting state: a counting backend plus its configuration.
///
/// Registered once per worker as `web::Data<RateLimiter>`; the middleware
/// reads it from app data, so degraded mode is simply "no data present".
pub struct RateLimiter {
    counter: Arc<dyn WindowCounter>,
    config: RateLimitConfig,
    now: NowFn,
}

impl RateLimiter {
    /// Create a limiter backed by a live Redis connection manager.
    pub fn new(conn: ConnectionManager, config: RateLimitConfig) -> Self {
        Self {
            counter: Arc::new(RedisWindowCounter { conn }),
            config,
            now: Arc::new(system_now),
        }
    }

    /// Create a limiter around an arbitrary counter backend (tests).
    pub fn with_counter<C: WindowCounter>(counter: C, config: RateLimitConfig) -> Self {
        Self {
            counter: Arc::new(counter),
            config,
            now: Arc::new(system_now),
        }
    }

    /// Create a limiter with a custom clock (tests): `now` returns the
    /// current unix epoch second.
    pub fn with_counter_and_clock<C: WindowCounter>(
        counter: C,
        config: RateLimitConfig,
        now: NowFn,
    ) -> Self {
        Self {
            counter: Arc::new(counter),
            config,
            now,
        }
    }

    /// Consume one request slot for `class` from the caller's bucket.
    ///
    /// Returns the decision, or [`RateLimitError`] when the backend fails —
    /// callers decide the failure policy (the middleware fails open).
    #[tracing::instrument(skip(self), fields(class = class.label()))]
    pub async fn check(
        &self,
        class: RateLimitClass,
        client_ip: &str,
    ) -> Result<RateLimitDecision, RateLimitError> {
        let (limit, window_secs) = class.budget(&self.config);
        let now_secs = (self.now)();
        // Config validation guarantees >= 1; max() keeps division safe even
        // for hand-built structs that bypass validation.
        let window_secs = window_secs.max(1);
        let window_start = now_secs - (now_secs % window_secs);
        let key = bucket_key(class, client_ip, window_start);

        let (count, ttl_secs) = self.counter.hit(&key, window_secs).await?;
        Ok(decision_from_count(limit, count, ttl_secs, now_secs))
    }
}

/// Transform factory for the rate-limiting middleware. Register globally:
/// enforcement is scoped to configured public prefixes internally.
#[derive(Debug, Clone, Copy, Default)]
pub struct RateLimit;

impl<S, B> Transform<S, ServiceRequest> for RateLimit
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S: 'static,
    S::Future: 'static,
    B: MessageBody + 'static,
{
    type Response = ServiceResponse;
    type Error = Error;
    type InitError = ();
    type Transform = RateLimitMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        // Arc so the async future in `call` can own a shareable handle to the
        // inner service without borrowing `&self` past `call`'s lifetime.
        ready(Ok(RateLimitMiddleware {
            service: Arc::new(service),
        }))
    }
}

/// Middleware service enforcing the configured budgets.
pub struct RateLimitMiddleware<S> {
    service: Arc<S>,
}

impl<S, B> Service<ServiceRequest> for RateLimitMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S: 'static,
    S::Future: 'static,
    B: MessageBody + 'static,
{
    type Response = ServiceResponse;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        // Snapshot everything needed before moving req into the future.
        let limiter_opt = req.app_data::<web::Data<RateLimiter>>().cloned();
        let path = req.path().to_string();
        let peer_addr = req.peer_addr().map(|a| a.ip().to_string());
        let xff = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        // Own a shareable handle to the inner service so the `'static` future
        // below never borrows `&self` (which cannot outlive `call`).
        let service = Arc::clone(&self.service);

        Box::pin(async move {
            let Some(limiter) = limiter_opt else {
                return service.call(req).await.map(|r| r.map_into_boxed_body());
            };
            if !limiter.config.enabled {
                return service.call(req).await.map(|r| r.map_into_boxed_body());
            }
            let Some(class) = classify_path(&path, &limiter.config) else {
                return service.call(req).await.map(|r| r.map_into_boxed_body());
            };
            let Some(client_ip) = extract_client_ip(
                peer_addr.as_deref(),
                xff.as_deref(),
                limiter.config.trust_xff,
            ) else {
                tracing::debug!("rate_limit: no identifiable peer address; passing through");
                return service.call(req).await.map(|r| r.map_into_boxed_body());
            };

            match limiter.check(class, &client_ip).await {
                Err(e) => {
                    // Fail-open: availability over strictness (docs/memory.md).
                    tracing::warn!(
                        error = %e,
                        path = %path,
                        "rate_limit: backend unavailable; allowing request"
                    );
                    service.call(req).await.map(|r| r.map_into_boxed_body())
                }
                Ok(decision) if decision.allowed => {
                    let mut res = service.call(req).await?.map_into_boxed_body();
                    apply_limit_headers(res.response_mut().headers_mut(), &decision);
                    Ok(res)
                }
                Ok(decision) => {
                    // Consume req directly — never clone the inner HttpRequest
                    // because actix panics when the downstream tries to call
                    // match_info_mut() on an Rc with refcount > 1.
                    tracing::info!(
                        path = %path,
                        client_ip = %client_ip,
                        class = class.label(),
                        "rate_limit: blocked request"
                    );
                    Ok(req.into_response(too_many_requests(&decision)))
                }
            }
        })
    }
}

/// Stamp the informational `X-RateLimit-*` headers onto a response.
fn apply_limit_headers(
    headers: &mut actix_web::http::header::HeaderMap,
    decision: &RateLimitDecision,
) {
    for (name, value) in [
        (HEADER_LIMIT, decision.limit.to_string()),
        (HEADER_REMAINING, decision.remaining.to_string()),
        (HEADER_RESET, decision.reset_epoch_secs.to_string()),
    ] {
        if let Ok(value) = header::HeaderValue::from_str(&value) {
            headers.insert(header::HeaderName::from_static(name), value);
        }
    }
}

/// Build the 429 response: standard headers plus a machine-readable body
/// shaped like every other JSON error in the API (`{"error": ...}`).
fn too_many_requests(decision: &RateLimitDecision) -> HttpResponse {
    let mut builder = HttpResponse::TooManyRequests();
    builder.insert_header((header::RETRY_AFTER, decision.retry_after_secs.to_string()));
    for (name, value) in [
        (HEADER_LIMIT, decision.limit.to_string()),
        (HEADER_REMAINING, decision.remaining.to_string()),
        (HEADER_RESET, decision.reset_epoch_secs.to_string()),
    ] {
        if let Ok(value) = header::HeaderValue::from_str(&value) {
            builder.insert_header((header::HeaderName::from_static(name), value));
        }
    }
    builder.json(json!({ "error": "rate limit exceeded" }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use actix_web::{App, http::StatusCode, test as awtest};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    const SECRET: &str = "test-secret-test-secret-test-secret-0000";

    fn stub_config() -> Config {
        Config {
            host: "127.0.0.1".into(),
            port: 8080,
            database_url: "mysql://test".into(),
            redis_url: "redis://test".into(),
            jwt_secret: SECRET.into(),
            jwt_expiry_secs: 3600,
            rate_limit: RateLimitConfig::default(),
        }
    }

    /// Scriptable counter backend recording how often it was consulted.
    #[allow(clippy::type_complexity)]
    struct MockCounter {
        outcome: Arc<Mutex<Box<dyn FnMut(&str) -> Result<(i64, i64), RateLimitError> + Send>>>,
        hits: Arc<AtomicUsize>,
    }

    impl MockCounter {
        fn always(count: i64, ttl: i64) -> Self {
            Self {
                outcome: Arc::new(Mutex::new(Box::new(move |_| Ok((count, ttl))))),
                hits: Arc::new(AtomicUsize::new(0)),
            }
        }

        /// Emulate the real Redis `INCR`: one counter per bucket key, each hit
        /// reporting one more count than the previous, with a fixed window
        /// `ttl`. A new window (new key) starts the counter afresh.
        fn incrementing(ttl: i64) -> Self {
            let counts = Arc::new(Mutex::new(std::collections::HashMap::<String, i64>::new()));
            let counts_clone = Arc::clone(&counts);
            Self {
                outcome: Arc::new(Mutex::new(Box::new(move |key| {
                    let mut counts = counts_clone.lock().expect("counts poisoned");
                    let count = counts.entry(key.to_string()).or_insert(0);
                    *count += 1;
                    Ok((*count, ttl))
                }))),
                hits: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn failing() -> Self {
            Self {
                outcome: Arc::new(Mutex::new(Box::new(|_| {
                    Err(RateLimitError::Redis(redis::RedisError::from(
                        std::io::Error::other("mock outage"),
                    )))
                }))),
                hits: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl WindowCounter for MockCounter {
        fn hit(&self, key: &str, _window_secs: u64) -> CounterFuture {
            let outcome = Arc::clone(&self.outcome);
            let hits = Arc::clone(&self.hits);
            let key = key.to_string();
            Box::pin(async move {
                hits.fetch_add(1, Ordering::SeqCst);
                let mut guard = outcome.lock().expect("mock lock poisoned");
                (guard)(&key)
            })
        }
    }

    /// Bridge sharing one `Arc<MockCounter>` between the app's limiter and
    /// the assertion handle returned to the test.
    #[derive(Clone)]
    struct CounterHandle(Arc<MockCounter>);

    impl WindowCounter for CounterHandle {
        fn hit(&self, key: &str, window_secs: u64) -> CounterFuture {
            let counter = Arc::clone(&self.0);
            let key = key.to_string();
            Box::pin(async move { counter.hit(&key, window_secs).await })
        }
    }

    /// Build the standard wrapped test app.
    ///
    /// Yields `(service, Arc<MockCounter>)`; the service type comes from
    /// `awtest::init_service` and cannot be named, so this is a macro.
    macro_rules! limited_app {
        ($counter:expr, $config:expr) => {{
            let counter = std::sync::Arc::new($counter);
            let handle = counter.clone();
            let app = awtest::init_service(
                App::new()
                    .wrap(RateLimit)
                    .app_data(web::Data::new(stub_config()))
                    .app_data(web::Data::new(RateLimiter::with_counter(
                        CounterHandle(handle),
                        $config,
                    )))
                    .route("/ws", web::get().to(|| async { "ws-ok" }))
                    .route("/v1/auth/ping", web::get().to(|| async { "auth-ok" }))
                    .route("/healthz", web::get().to(|| async { "healthy" })),
            )
            .await;
            (app, counter)
        }};
    }

    /// GET `$uri` from peer ip `203.0.113.$ip` — per-test octets keep Redis
    /// buckets isolated across parallel integration runs.
    macro_rules! get_from {
        ($app:expr, $ip:expr, $uri:expr) => {{
            let addr = format!("203.0.113.{}:50000", $ip)
                .parse::<std::net::SocketAddr>()
                .expect("valid addr");
            let req = awtest::TestRequest::get()
                .uri($uri)
                .peer_addr(addr)
                .to_request();
            awtest::call_service(&$app, req)
        }};
    }

    /// Default-class budget 3, auth-class budget 1 — small enough to burst.
    fn default_cfg() -> RateLimitConfig {
        RateLimitConfig {
            requests: 3,
            auth_requests: 1,
            ..RateLimitConfig::default()
        }
    }

    #[test]
    fn key_format_includes_class_ip_window() {
        assert_eq!(
            bucket_key(RateLimitClass::Auth, "1.2.3.4", 1720000000),
            "rl:auth:1.2.3.4:1720000000"
        );
        assert_eq!(
            bucket_key(RateLimitClass::Default, "10.0.0.9", 42),
            "rl:default:10.0.0.9:42"
        );
    }

    #[test]
    fn path_classification_matches_spec() {
        let cfg = RateLimitConfig::default();
        assert_eq!(
            classify_path("/v1/auth/login", &cfg),
            Some(RateLimitClass::Auth)
        );
        assert_eq!(
            classify_path("/v1/auth/register", &cfg),
            Some(RateLimitClass::Auth)
        );
        // Legacy alias mount from the 0.4.1 parity pass.
        assert_eq!(
            classify_path("/auth/register", &cfg),
            Some(RateLimitClass::Auth)
        );
        // Segment boundary: must not swallow /v1/authentication-style paths.
        assert_eq!(classify_path("/v1/authentication", &cfg), None);
        assert_eq!(classify_path("/ws", &cfg), Some(RateLimitClass::Default));
        // Liveness probes are never limited.
        assert_eq!(classify_path("/healthz", &cfg), None);
        assert_eq!(classify_path("/v1/saves", &cfg), None);
    }

    #[test]
    fn custom_public_prefixes_classify_as_default() {
        let cfg = RateLimitConfig {
            public_prefixes: vec!["/v1/events".into()],
            ..RateLimitConfig::default()
        };
        assert_eq!(
            classify_path("/v1/events", &cfg),
            Some(RateLimitClass::Default)
        );
        assert_eq!(
            classify_path("/v1/events/batch", &cfg),
            Some(RateLimitClass::Default)
        );
        assert_eq!(classify_path("/v1/eventz", &cfg), None);
    }

    #[test]
    fn xff_disabled_uses_peer_addr() {
        assert_eq!(
            extract_client_ip(Some("198.51.100.5"), Some("10.0.0.1"), false),
            Some("198.51.100.5".to_string())
        );
    }

    #[test]
    fn xff_enabled_uses_first_hop() {
        assert_eq!(
            extract_client_ip(Some("198.51.100.5"), Some("10.0.0.1, 20.0.0.2"), true),
            Some("10.0.0.1".to_string())
        );
        assert_eq!(
            extract_client_ip(Some("198.51.100.5"), Some(" 10.0.0.1 "), true),
            Some("10.0.0.1".to_string())
        );
    }

    #[test]
    fn missing_or_untrusted_xff_falls_back_to_peer_addr() {
        assert_eq!(
            extract_client_ip(Some("198.51.100.5"), None, true),
            Some("198.51.100.5".to_string())
        );
        assert_eq!(extract_client_ip(None, None, true), None);
        assert_eq!(extract_client_ip(None, Some("10.0.0.1"), false), None);
        // Empty first hop cannot become a bucket key.
        assert_eq!(extract_client_ip(None, Some(" , 20.0.0.2"), true), None);
    }

    #[test]
    fn headers_math_clamps_remaining_and_reset() {
        let now = 1_720_000_000;
        // Over-limit: remaining clamps to 0, retry-after floors at 1.
        let blocked = decision_from_count(5, 7, 0, now);
        assert!(!blocked.allowed);
        assert_eq!(blocked.remaining, 0);
        assert_eq!(blocked.retry_after_secs, 1);
        assert_eq!(blocked.reset_epoch_secs, now + 1);

        // Under limit with sane ttl.
        let ok = decision_from_count(5, 3, 41, now);
        assert!(ok.allowed);
        assert_eq!(ok.remaining, 2);
        assert_eq!(ok.retry_after_secs, 0);
        assert_eq!(ok.reset_epoch_secs, now + 41);

        // Negative backend ttl cannot advertise a past reset.
        let odd = decision_from_count(5, 6, -1, now);
        assert_eq!(odd.retry_after_secs, 1);
    }

    #[actix_web::test]
    async fn disabled_limiter_passes_all_requests_without_touching_backend() {
        let (app, counter) = limited_app!(
            MockCounter::always(999, 60),
            RateLimitConfig {
                enabled: false,
                ..default_cfg()
            }
        );

        for octet in 1..=10 {
            let res = get_from!(app, octet, "/ws").await;
            assert_eq!(res.status(), StatusCode::OK);
        }
        assert_eq!(
            counter.hits.load(Ordering::SeqCst),
            0,
            "disabled limiter must not consult the backend"
        );
    }

    #[actix_web::test]
    async fn missing_limiter_data_passes_through() {
        let app = awtest::init_service(
            App::new()
                .wrap(RateLimit)
                .route("/ws", web::get().to(|| async { "degraded-ok" })),
        )
        .await;
        let res =
            awtest::call_service(&app, awtest::TestRequest::get().uri("/ws").to_request()).await;
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[actix_web::test]
    async fn redis_error_fails_open() {
        let (app, counter) = limited_app!(MockCounter::failing(), default_cfg());

        let res = get_from!(app, 20, "/ws").await;
        assert_eq!(
            res.status(),
            StatusCode::OK,
            "backend outage must not block"
        );
        assert_eq!(
            counter.hits.load(Ordering::SeqCst),
            1,
            "failure path still consults the backend"
        );
    }

    #[actix_web::test]
    async fn under_limit_passes_with_headers() {
        let (app, _) = limited_app!(
            MockCounter::always(1, 59),
            RateLimitConfig {
                requests: 5,
                ..default_cfg()
            }
        );

        let res = get_from!(app, 20, "/ws").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.response().headers().get(HEADER_LIMIT).unwrap(), "5");
        assert_eq!(res.response().headers().get(HEADER_REMAINING).unwrap(), "4");
        assert!(res.response().headers().get(HEADER_RESET).is_some());
    }

    #[actix_web::test]
    async fn exceeds_limit_after_budget_depletes() {
        let (app, counter) = limited_app!(
            MockCounter::incrementing(59),
            RateLimitConfig {
                requests: 3,
                ..default_cfg()
            }
        );

        // Same peer IP throughout, so every request shares one bucket.
        for _ in 0..3 {
            let res = get_from!(app, 40, "/ws").await;
            assert_eq!(res.status(), StatusCode::OK);
        }
        assert_eq!(counter.hits.load(Ordering::SeqCst), 3);

        // Fourth hit pushes the fixed-window count past the budget -> 429.
        let blocked = get_from!(app, 40, "/ws").await;
        assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);
        let headers = blocked.response().headers();
        assert_eq!(headers.get(HEADER_LIMIT).unwrap(), "3");
        assert_eq!(headers.get(HEADER_REMAINING).unwrap(), "0");
        assert_eq!(headers.get(header::RETRY_AFTER).unwrap(), "59");
        assert!(headers.get(HEADER_RESET).is_some());

        let body: serde_json::Value = awtest::read_body_json(blocked).await;
        assert_eq!(body["error"], "rate limit exceeded");
        assert_eq!(counter.hits.load(Ordering::SeqCst), 4);
    }

    #[actix_web::test]
    async fn window_resets_after_retry_after_elapses() {
        // Start on a 60s-aligned boundary so one `Retry-After` advance lands
        // exactly on the next window.
        let now = Arc::new(Mutex::new(1_720_000_020u64));
        let clock = Arc::clone(&now);
        let counter = Arc::new(MockCounter::incrementing(60));
        let handle = Arc::clone(&counter);
        let app = awtest::init_service(
            App::new()
                .wrap(RateLimit)
                .app_data(web::Data::new(stub_config()))
                .app_data(web::Data::new(RateLimiter::with_counter_and_clock(
                    CounterHandle(handle),
                    RateLimitConfig {
                        requests: 3,
                        window_secs: 60,
                        ..default_cfg()
                    },
                    Arc::new(move || *clock.lock().expect("clock poisoned")),
                )))
                .route("/ws", web::get().to(|| async { "ws-ok" })),
        )
        .await;

        // Deplete the budget inside the first window.
        for _ in 0..3 {
            let res = get_from!(app, 50, "/ws").await;
            assert_eq!(res.status(), StatusCode::OK);
        }
        let blocked = get_from!(app, 50, "/ws").await;
        assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);
        let retry_after: u64 = blocked
            .response()
            .headers()
            .get(header::RETRY_AFTER)
            .unwrap()
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(retry_after, 60);
        assert_eq!(counter.hits.load(Ordering::SeqCst), 4);

        // Wait out the window: the bucket rolls to a fresh key and the
        // counter starts over, so the next request is allowed. One second
        // short of `Retry-After` the old window is still in force.
        *now.lock().expect("clock poisoned") += retry_after - 1;
        let still_blocked = get_from!(app, 50, "/ws").await;
        assert_eq!(
            still_blocked.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "budget must hold until the window elapses"
        );
        assert_eq!(counter.hits.load(Ordering::SeqCst), 5);

        *now.lock().expect("clock poisoned") += 1;
        let res = get_from!(app, 50, "/ws").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(counter.hits.load(Ordering::SeqCst), 6);
    }

    #[actix_web::test]
    async fn four_twenty_nine_body_and_headers() {
        let (app, _) = limited_app!(
            MockCounter::always(4, 37),
            RateLimitConfig {
                requests: 3,
                ..default_cfg()
            }
        );

        let res = get_from!(app, 20, "/ws").await;
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        let headers = res.response().headers();
        assert_eq!(headers.get(header::RETRY_AFTER).unwrap(), "37");
        assert_eq!(headers.get(HEADER_LIMIT).unwrap(), "3");
        assert_eq!(headers.get(HEADER_REMAINING).unwrap(), "0");
        assert!(headers.get(HEADER_RESET).is_some());

        let body: serde_json::Value = awtest::read_body_json(res).await;
        assert_eq!(body["error"], "rate limit exceeded");
    }

    #[actix_web::test]
    async fn excluded_paths_never_hit_the_backend() {
        let (app, counter) = limited_app!(MockCounter::always(999, 60), default_cfg());

        let healthz = get_from!(app, 30, "/healthz").await;
        assert_eq!(healthz.status(), StatusCode::OK);
        assert!(healthz.response().headers().get(HEADER_LIMIT).is_none());
        let saves_like = get_from!(app, 31, "/v1/saves").await;
        assert_ne!(saves_like.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            counter.hits.load(Ordering::SeqCst),
            0,
            "non-public paths must bypass the counter entirely"
        );
    }

    #[actix_web::test]
    async fn no_peer_addr_passes_through() {
        let (app, counter) = limited_app!(MockCounter::always(999, 60), default_cfg());

        // No peer addr → no bucket key → pass through untouched.
        let req = awtest::TestRequest::get().uri("/ws").to_request();
        let res = awtest::call_service(&app, req).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(counter.hits.load(Ordering::SeqCst), 0);
    }
}
