//! Application configuration loaded from environment.
//!
//! Centralises all runtime configuration so the rest of the codebase can
//! depend on a typed `Config` rather than scattered `std::env::var` calls.

use anyhow::{Context, Result, anyhow};

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

        Ok(Self {
            host,
            port,
            database_url,
            redis_url,
            jwt_secret,
            jwt_expiry_secs,
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
}
