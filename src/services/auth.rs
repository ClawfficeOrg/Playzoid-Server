//! Authentication primitives: password hashing (Argon2id) and JWT issue/verify.
//!
//! See `memory/projects/playzoid-server/project.md` for the password-hash
//! decision (Argon2id, 2026-04-30) and `docs/TODO.md` Phase 0.2-1..0.2-3 for
//! the consuming endpoints. This module is intentionally I/O-free so it can
//! be unit-tested without a database or HTTP runtime.
//!
//! `dead_code` is allowed at module scope because the JWT/password helpers
//! are introduced ahead of their first consumers (the `/auth/login` and
//! `/auth/register` handlers in the next PR).
#![allow(dead_code)]

use anyhow::{Context, Result, anyhow};
use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};
use jsonwebtoken::{
    Algorithm, DecodingKey, EncodingKey, Header, TokenData, Validation, decode, encode,
};
use serde::{Deserialize, Serialize};

/// Minimum acceptable plaintext password length. Mirrored in `validator`
/// constraints on register/login DTOs (added in 0.2-1/0.2-3).
pub const MIN_PASSWORD_LEN: usize = 8;

/// Maximum plaintext password length. Argon2 itself has no hard cap, but we
/// reject pathological inputs early to avoid trivial DoS via huge hashes.
pub const MAX_PASSWORD_LEN: usize = 1024;

/// JWT claims. `sub` is the player's `public_id` (CHAR(36) UUID), never the
/// internal `BIGINT` id, so tokens never leak primary keys.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Claims {
    /// Subject — the player's `public_id`.
    pub sub: String,
    /// Issued-at, seconds since the UNIX epoch.
    pub iat: i64,
    /// Expiry, seconds since the UNIX epoch.
    pub exp: i64,
}

/// Hash a plaintext password with Argon2id and the OS RNG salt.
///
/// Returns the canonical PHC string, e.g.
/// `$argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>`. Store this verbatim in
/// `players.password_hash`.
pub fn hash_password(plain: &str) -> Result<String> {
    if plain.len() < MIN_PASSWORD_LEN {
        return Err(anyhow!(
            "password too short (min {} chars)",
            MIN_PASSWORD_LEN
        ));
    }
    if plain.len() > MAX_PASSWORD_LEN {
        return Err(anyhow!(
            "password too long (max {} chars)",
            MAX_PASSWORD_LEN
        ));
    }

    let salt = SaltString::generate(&mut OsRng);
    let argon = Argon2::default();
    let hash = argon
        .hash_password(plain.as_bytes(), &salt)
        .map_err(|e| anyhow!("argon2 hash failed: {e}"))?
        .to_string();
    Ok(hash)
}

/// Verify a plaintext password against a stored PHC hash.
///
/// Returns `Ok(true)` on match, `Ok(false)` on mismatch. `Err` is reserved
/// for malformed hashes — callers should treat that as a server-side
/// integrity issue, not a login failure.
pub fn verify_password(plain: &str, phc_hash: &str) -> Result<bool> {
    let parsed =
        PasswordHash::new(phc_hash).map_err(|e| anyhow!("invalid stored password hash: {e}"))?;
    match Argon2::default().verify_password(plain.as_bytes(), &parsed) {
        Ok(()) => Ok(true),
        Err(argon2::password_hash::Error::Password) => Ok(false),
        Err(e) => Err(anyhow!("argon2 verify failed: {e}")),
    }
}

/// Issue a signed JWT for the given subject (`public_id`) with `ttl_secs`
/// validity from now. HS256 is used because `jwt_secret` is symmetric (see
/// `Config::jwt_secret`).
pub fn issue_jwt(secret: &str, subject: &str, ttl_secs: u64) -> Result<String> {
    let now = chrono::Utc::now().timestamp();
    let exp = now
        .checked_add(i64::try_from(ttl_secs).context("ttl_secs out of range for i64")?)
        .ok_or_else(|| anyhow!("token exp overflowed i64"))?;
    let claims = Claims {
        sub: subject.to_string(),
        iat: now,
        exp,
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .context("failed to sign JWT")
}

/// Verify a JWT and return its claims.
///
/// Validates signature, algorithm (HS256), and `exp`. Tokens with `exp` in
/// the past are rejected.
pub fn verify_jwt(secret: &str, token: &str) -> Result<Claims> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = 0;
    let data: TokenData<Claims> = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .context("JWT verification failed")?;
    Ok(data.claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "test-secret-test-secret-test-secret-0000"; // >= 32 chars

    #[test]
    fn hash_then_verify_round_trip() {
        let phc = hash_password("correct horse battery staple").expect("hash");
        assert!(phc.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery staple", &phc).expect("verify"));
        assert!(!verify_password("wrong password!", &phc).expect("verify"));
    }

    #[test]
    fn hash_rejects_too_short() {
        let err = hash_password("short").expect_err("should reject");
        assert!(err.to_string().contains("too short"));
    }

    #[test]
    fn verify_rejects_malformed_hash() {
        let err = verify_password("anything-goes-here", "not-a-phc-string").expect_err("malformed");
        assert!(err.to_string().contains("invalid stored password hash"));
    }

    #[test]
    fn jwt_round_trip_succeeds() {
        let token = issue_jwt(SECRET, "00000000-0000-4000-8000-000000000001", 60).expect("issue");
        let claims = verify_jwt(SECRET, &token).expect("verify");
        assert_eq!(claims.sub, "00000000-0000-4000-8000-000000000001");
        assert!(claims.exp > claims.iat);
    }

    #[test]
    fn jwt_rejects_wrong_secret() {
        let token = issue_jwt(SECRET, "abc", 60).expect("issue");
        let err = verify_jwt("a-different-secret-of-sufficient-length-xx", &token)
            .expect_err("verify should fail");
        assert!(err.to_string().contains("JWT verification failed"));
    }

    #[test]
    fn jwt_rejects_expired() {
        // ttl=0 means iat == exp; with leeway=0 this is immediately expired.
        let token = issue_jwt(SECRET, "abc", 0).expect("issue");
        // Sleep one second to ensure exp is firmly in the past.
        std::thread::sleep(std::time::Duration::from_secs(1));
        let err = verify_jwt(SECRET, &token).expect_err("expired token rejected");
        assert!(err.to_string().contains("JWT verification failed"));
    }
}
