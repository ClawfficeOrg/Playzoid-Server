//! Application configuration loaded from environment.
//!
//! Centralises all runtime configuration so the rest of the codebase can
//! depend on a typed `Config` rather than scattered `std::env::var` calls.

use anyhow::{Context, Result, anyhow};

/// Rate limiting configuration (task 0.4.8).
///
/// Fixed-window counters are kept in Redis; limits are expressed as
/// `(requests, window_secs)` pairs per request class:
///
/// - `auth` — the tightest budget, applied to credential endpoints
///   (`/v1/auth/**`, legacy `/auth/**`) to slow brute-force attempts.
/// - `default` — the general budget for every other rate-limited public
///   route (e.g. the `/ws` handshake).
///
/// All values load from `RATE_LIMIT_*` environment variables and validate
/// at startup so nonsense configuration fails fast.
#[derive(Debug, Clone, PartialEq)]
pub struct RateLimitConfig {
    /// Master switch. When `false` no limiter is registered and requests
    /// pass through untouched.
    pub enabled: bool,
    /// Default-class request budget per window.
    pub requests: u32,
    /// Default-class window length in seconds.
    pub window_secs: u64,
    /// Auth-class request budget per window.
    pub auth_requests: u32,
    /// Auth-class window length in seconds.
    pub auth_window_secs: u64,
    /// Path prefixes subject to default-class limiting. Paths under the
    /// auth prefixes (`/v1/auth`, `/auth`) always use the tighter auth
    /// budget instead. `/healthz` must never be listed here — liveness
    /// probes may not be rate limited.
    pub public_prefixes: Vec<String>,
    /// Trust `X-Forwarded-For` for client identification instead of the
    /// socket peer address. Only enable behind a proxy that overwrites
    /// the header; otherwise clients can spoof their bucket.
    pub trust_xff: bool,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            requests: 100,
            window_secs: 60,
            auth_requests: 10,
            auth_window_secs: 60,
            public_prefixes: vec!["/v1/auth".into(), "/auth".into(), "/ws".into()],
            trust_xff: false,
        }
    }
}

impl RateLimitConfig {
    /// Upper bound accepted for any window length (seconds) — guards against
    /// a typo freezing buckets for hours.
    const MAX_WINDOW_SECS: u64 = 3600;

    /// Load rate-limit settings from environment.
    ///
    /// Unset variables fall back to [`RateLimitConfig::default`] values;
    /// present-but-invalid values return `Err` so startup fails fast.
    pub fn from_env() -> Result<Self> {
        let mut cfg = Self::default();

        if std::env::var("RATE_LIMIT_ENABLED").is_ok() {
            cfg.enabled = parse_bool("RATE_LIMIT_ENABLED")?;
        }
        if std::env::var("RATE_LIMIT_REQUESTS").is_ok() {
            cfg.requests = std::env::var("RATE_LIMIT_REQUESTS")
                .context("reading RATE_LIMIT_REQUESTS")?
                .parse::<u32>()
                .context("RATE_LIMIT_REQUESTS must be a valid u32")?;
        }
        if std::env::var("RATE_LIMIT_WINDOW_SECS").is_ok() {
            cfg.window_secs = std::env::var("RATE_LIMIT_WINDOW_SECS")
                .context("reading RATE_LIMIT_WINDOW_SECS")?
                .parse::<u64>()
                .context("RATE_LIMIT_WINDOW_SECS must be a valid u64")?;
        }
        if std::env::var("RATE_LIMIT_AUTH_REQUESTS").is_ok() {
            cfg.auth_requests = std::env::var("RATE_LIMIT_AUTH_REQUESTS")
                .context("reading RATE_LIMIT_AUTH_REQUESTS")?
                .parse::<u32>()
                .context("RATE_LIMIT_AUTH_REQUESTS must be a valid u32")?;
        }
        if std::env::var("RATE_LIMIT_AUTH_WINDOW_SECS").is_ok() {
            cfg.auth_window_secs = std::env::var("RATE_LIMIT_AUTH_WINDOW_SECS")
                .context("reading RATE_LIMIT_AUTH_WINDOW_SECS")?
                .parse::<u64>()
                .context("RATE_LIMIT_AUTH_WINDOW_SECS must be a valid u64")?;
        }
        if std::env::var("RATE_LIMIT_PUBLIC_PREFIXES").is_ok() {
            let raw = std::env::var("RATE_LIMIT_PUBLIC_PREFIXES")
                .context("reading RATE_LIMIT_PUBLIC_PREFIXES")?;
            cfg.public_prefixes = raw
                .split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect();
        }
        if std::env::var("RATE_LIMIT_TRUST_XFF").is_ok() {
            cfg.trust_xff = parse_bool("RATE_LIMIT_TRUST_XFF")?;
        }

        cfg.validate()?;
        Ok(cfg)
    }

    /// Reject nonsensical limits/windows before the server starts.
    fn validate(&self) -> Result<()> {
        if self.requests == 0 {
            return Err(anyhow!("RATE_LIMIT_REQUESTS must be at least 1"));
        }
        if self.auth_requests == 0 {
            return Err(anyhow!("RATE_LIMIT_AUTH_REQUESTS must be at least 1"));
        }
        if self.window_secs == 0 || self.window_secs > Self::MAX_WINDOW_SECS {
            return Err(anyhow!(
                "RATE_LIMIT_WINDOW_SECS must be between 1 and {}",
                Self::MAX_WINDOW_SECS
            ));
        }
        if self.auth_window_secs == 0 || self.auth_window_secs > Self::MAX_WINDOW_SECS {
            return Err(anyhow!(
                "RATE_LIMIT_AUTH_WINDOW_SECS must be between 1 and {}",
                Self::MAX_WINDOW_SECS
            ));
        }
        Ok(())
    }
}

/// Parse a strict boolean env var (`true`/`false`, case-insensitive, or `1`/`0`).
fn parse_bool(var: &str) -> Result<bool> {
    match std::env::var(var)
        .context(format!("reading {var}"))?
        .to_lowercase()
        .as_str()
    {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        other => Err(anyhow!("{var} must be true or false (got {other})")),
    }
}

/// Runtime configuration loaded from environment variables (and optionally
/// an `.env` file via [`dotenvy::dotenv`]).
///
/// `Config::from_env` is the single entry-point and validates inputs at
/// startup so misconfiguration fails fast.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields are consumed in R-7 (sqlx pool), R-8 (migrations), and Phase 0.2 (auth).
pub struct Config {
    pub host: String,
    pub port: u16,
    pub database_url: String,
    pub redis_url: String,
    pub jwt_secret: String,
    pub jwt_expiry_secs: u64,
    pub rate_limit: RateLimitConfig,
}

impl Config {
    /// Minimum acceptable JWT secret length, per `docs/GUIDELINES.md` security policy.
    pub const MIN_JWT_SECRET_LEN: usize = 32;

    /// Load configuration from process environment.
    ///
    /// Returns `Err` if a required variable is missing or invalid.
    pub fn from_env() -> Result<Self> {
        let host = std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let port = std::env::var("PORT")
            .unwrap_or_else(|_| "8080".to_string())
            .parse::<u16>()
            .context("PORT must be a valid u16")?;

        let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;
        let redis_url = std::env::var("REDIS_URL").context("REDIS_URL is required")?;

        let jwt_secret = std::env::var("JWT_SECRET").context("JWT_SECRET is required")?;
        if jwt_secret.len() < Self::MIN_JWT_SECRET_LEN {
            return Err(anyhow!(
                "JWT_SECRET must be at least {} characters (got {})",
                Self::MIN_JWT_SECRET_LEN,
                jwt_secret.len()
            ));
        }

        let jwt_expiry_secs = std::env::var("JWT_EXPIRY_SECONDS")
            .unwrap_or_else(|_| "3600".to_string())
            .parse::<u64>()
            .context("JWT_EXPIRY_SECONDS must be a valid u64")?;

        let rate_limit = RateLimitConfig::from_env()?;

        Ok(Self {
            host,
            port,
            database_url,
            redis_url,
            jwt_secret,
            jwt_expiry_secs,
            rate_limit,
        })
    }

    /// Convenience accessor for the bind socket string `host:port`.
    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: set all required env vars for a test.
    /// Tests run serially via `--test-threads=1` is not assumed, so each test
    /// uses unique-enough values; we hold the lock implicitly by setting the
    /// same canonical values everywhere.
    fn set_required(jwt: &str) {
        // SAFETY: tests in this module are gated behind a Mutex below.
        unsafe {
            std::env::set_var("DATABASE_URL", "mysql://u:p@localhost/db");
            std::env::set_var("REDIS_URL", "redis://localhost:6379");
            std::env::set_var("JWT_SECRET", jwt);
            std::env::remove_var("HOST");
            std::env::remove_var("PORT");
            std::env::remove_var("JWT_EXPIRY_SECONDS");
            clear_rate_limit_env();
        }
    }

    /// Clear every `RATE_LIMIT_*` variable so defaults apply.
    fn clear_rate_limit_env() {
        // SAFETY: all callers hold ENV_LOCK.
        unsafe {
            for var in [
                "RATE_LIMIT_ENABLED",
                "RATE_LIMIT_REQUESTS",
                "RATE_LIMIT_WINDOW_SECS",
                "RATE_LIMIT_AUTH_REQUESTS",
                "RATE_LIMIT_AUTH_WINDOW_SECS",
                "RATE_LIMIT_PUBLIC_PREFIXES",
                "RATE_LIMIT_TRUST_XFF",
            ] {
                std::env::remove_var(var);
            }
        }
    }

    // Serialise env access across tests in this module.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn from_env_happy_path_uses_defaults() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_required("a".repeat(32).as_str());

        let cfg = Config::from_env().expect("config loads");
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 8080);
        assert_eq!(cfg.jwt_expiry_secs, 3600);
        assert_eq!(cfg.bind_addr(), "127.0.0.1:8080");
    }

    #[test]
    fn from_env_rejects_short_jwt_secret() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_required("too-short");

        let err = Config::from_env().expect_err("short secret should fail");
        assert!(err.to_string().contains("JWT_SECRET"));
    }

    #[test]
    fn from_env_requires_database_url() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_required("a".repeat(32).as_str());
        // SAFETY: guarded by ENV_LOCK
        unsafe {
            std::env::remove_var("DATABASE_URL");
        }

        let err = Config::from_env().expect_err("missing DATABASE_URL should fail");
        assert!(err.to_string().contains("DATABASE_URL"));
    }

    #[test]
    fn rate_limit_defaults_when_env_unset() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_required("a".repeat(32).as_str());

        let cfg = Config::from_env().expect("config loads").rate_limit;
        assert_eq!(cfg, RateLimitConfig::default());
        assert!(cfg.enabled);
        assert_eq!(cfg.requests, 100);
        assert_eq!(cfg.window_secs, 60);
        assert_eq!(cfg.auth_requests, 10);
        assert_eq!(cfg.auth_window_secs, 60);
        assert!(!cfg.trust_xff);
        assert_eq!(cfg.public_prefixes, vec!["/v1/auth", "/auth", "/ws"]);
    }

    #[test]
    fn rate_limit_parses_custom_env() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_required("a".repeat(32).as_str());
        // SAFETY: guarded by ENV_LOCK
        unsafe {
            std::env::set_var("RATE_LIMIT_ENABLED", "true");
            std::env::set_var("RATE_LIMIT_REQUESTS", "250");
            std::env::set_var("RATE_LIMIT_WINDOW_SECS", "30");
            std::env::set_var("RATE_LIMIT_AUTH_REQUESTS", "5");
            std::env::set_var("RATE_LIMIT_AUTH_WINDOW_SECS", "120");
            std::env::set_var("RATE_LIMIT_PUBLIC_PREFIXES", " /ws , /v1/events , ");
            std::env::set_var("RATE_LIMIT_TRUST_XFF", "true");
        }

        let cfg = Config::from_env().expect("config loads").rate_limit;
        assert!(cfg.enabled);
        assert_eq!(cfg.requests, 250);
        assert_eq!(cfg.window_secs, 30);
        assert_eq!(cfg.auth_requests, 5);
        assert_eq!(cfg.auth_window_secs, 120);
        assert!(cfg.trust_xff);
        // Entries trim; empty segments drop.
        assert_eq!(cfg.public_prefixes, vec!["/ws", "/v1/events"]);
    }

    #[test]
    fn rate_limit_can_disable_via_env() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_required("a".repeat(32).as_str());
        // SAFETY: guarded by ENV_LOCK
        unsafe {
            std::env::set_var("RATE_LIMIT_ENABLED", "false");
        }

        let cfg = Config::from_env().expect("config loads").rate_limit;
        assert!(!cfg.enabled);
    }

    #[test]
    fn rate_limit_rejects_zero_requests() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_required("a".repeat(32).as_str());
        // SAFETY: guarded by ENV_LOCK
        unsafe {
            std::env::set_var("RATE_LIMIT_REQUESTS", "0");
        }

        let err = Config::from_env().expect_err("zero requests should fail");
        assert!(err.to_string().contains("RATE_LIMIT_REQUESTS"));
    }

    #[test]
    fn rate_limit_rejects_zero_auth_requests() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_required("a".repeat(32).as_str());
        // SAFETY: guarded by ENV_LOCK
        unsafe {
            std::env::set_var("RATE_LIMIT_AUTH_REQUESTS", "0");
        }

        let err = Config::from_env().expect_err("zero auth requests should fail");
        assert!(err.to_string().contains("RATE_LIMIT_AUTH_REQUESTS"));
    }

    #[test]
    fn rate_limit_rejects_zero_window() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_required("a".repeat(32).as_str());
        // SAFETY: guarded by ENV_LOCK
        unsafe {
            std::env::set_var("RATE_LIMIT_WINDOW_SECS", "0");
        }

        let err = Config::from_env().expect_err("zero window should fail");
        assert!(err.to_string().contains("RATE_LIMIT_WINDOW_SECS"));
    }

    #[test]
    fn rate_limit_rejects_oversized_window() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_required("a".repeat(32).as_str());
        // SAFETY: guarded by ENV_LOCK
        unsafe {
            std::env::set_var("RATE_LIMIT_AUTH_WINDOW_SECS", "3601");
        }

        let err = Config::from_env().expect_err("oversized window should fail");
        assert!(err.to_string().contains("RATE_LIMIT_AUTH_WINDOW_SECS"));
    }

    #[test]
    fn rate_limit_rejects_bad_bool() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_required("a".repeat(32).as_str());
        // SAFETY: guarded by ENV_LOCK
        unsafe {
            std::env::set_var("RATE_LIMIT_TRUST_XFF", "maybe");
        }

        let err = Config::from_env().expect_err("bad bool should fail");
        assert!(err.to_string().contains("RATE_LIMIT_TRUST_XFF"));
    }
}
