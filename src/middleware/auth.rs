//! JWT authentication extractor for actix-web handler functions.
//!
//! Add [`AuthenticatedUser`] as a handler parameter to require a valid
//! `Authorization: Bearer <token>` header. The extractor verifies the token
//! signature and expiry using the app's [`Config`] and returns HTTP 401
//! automatically on any failure — the handler body never runs.
//!
//! # Example
//!
//! ```rust,ignore
//! use crate::middleware::auth::AuthenticatedUser;
//!
//! async fn protected(user: AuthenticatedUser) -> HttpResponse {
//!     HttpResponse::Ok().json(serde_json::json!({ "player": user.player_public_id }))
//! }
//! ```

use crate::{config::Config, services::auth as auth_svc};
use actix_web::{
    Error, FromRequest, HttpRequest,
    dev::Payload,
    error::{ErrorInternalServerError, ErrorUnauthorized},
    http::header,
    web,
};
pub use auth_svc::Claims;
use std::future::{Ready, ready};

/// The authenticated caller, extracted from a valid `Authorization: Bearer <jwt>` header.
///
/// Add this as a parameter to any handler that requires authentication. The
/// extractor automatically returns HTTP 401 when:
/// - The `Authorization` header is absent.
/// - The header does not use the `Bearer` scheme.
/// - The token signature is invalid.
/// - The token has expired.
///
/// On success, `player_public_id` contains the player's UUID and `claims`
/// carries the full decoded [`Claims`] struct.
#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    /// The player's `public_id` (UUID) — safe to surface in API responses.
    pub player_public_id: String,
    /// Full decoded JWT claims, including `iat` and `exp` timestamps.
    /// Used by handlers that need expiry info or the raw subject claim.
    #[allow(dead_code)]
    pub claims: Claims,
}

impl FromRequest for AuthenticatedUser {
    type Error = Error;
    type Future = Ready<Result<Self, Error>>;

    fn from_request(req: &HttpRequest, _payload: &mut Payload) -> Self::Future {
        let token = match extract_bearer_token(req) {
            Ok(t) => t,
            Err(e) => return ready(Err(e)),
        };

        let cfg = match req.app_data::<web::Data<Config>>() {
            Some(c) => c,
            None => {
                tracing::error!("AuthenticatedUser extractor: Config not found in app data");
                return ready(Err(ErrorInternalServerError("server misconfigured")));
            }
        };

        match auth_svc::verify_jwt(&cfg.jwt_secret, &token) {
            Ok(claims) => ready(Ok(AuthenticatedUser {
                player_public_id: claims.sub.clone(),
                claims,
            })),
            Err(e) => {
                tracing::debug!(error = %e, "JWT verification failed");
                ready(Err(ErrorUnauthorized("invalid or expired token")))
            }
        }
    }
}

/// Extract the raw token string from `Authorization: Bearer <token>`.
fn extract_bearer_token(req: &HttpRequest) -> Result<String, Error> {
    let header_value = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ErrorUnauthorized("Authorization header required"))?;

    let token = header_value
        .strip_prefix("Bearer ")
        .ok_or_else(|| ErrorUnauthorized("Authorization header must use Bearer scheme"))?;

    if token.is_empty() {
        return Err(ErrorUnauthorized("Bearer token must not be empty"));
    }

    Ok(token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::auth as auth_svc;
    use actix_web::{App, HttpResponse, http::StatusCode, test, web};

    const SECRET: &str = "test-secret-test-secret-test-secret-0000";

    fn stub_config() -> Config {
        Config {
            host: "127.0.0.1".into(),
            port: 8080,
            database_url: "mysql://test".into(),
            redis_url: "redis://test".into(),
            jwt_secret: SECRET.into(),
            jwt_expiry_secs: 3600,
        }
    }

    /// A minimal protected handler used only in these tests.
    async fn protected_handler(user: AuthenticatedUser) -> HttpResponse {
        HttpResponse::Ok().json(serde_json::json!({ "player_id": user.player_public_id }))
    }

    fn app_with_auth() -> App<
        impl actix_web::dev::ServiceFactory<
            actix_web::dev::ServiceRequest,
            Config = (),
            Response = actix_web::dev::ServiceResponse,
            Error = actix_web::Error,
            InitError = (),
        >,
    > {
        App::new()
            .app_data(web::Data::new(stub_config()))
            .route("/protected", web::get().to(protected_handler))
    }

    #[actix_web::test]
    async fn missing_auth_header_returns_401() {
        let app = test::init_service(app_with_auth()).await;
        let req = test::TestRequest::get().uri("/protected").to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn non_bearer_scheme_returns_401() {
        let app = test::init_service(app_with_auth()).await;
        let req = test::TestRequest::get()
            .uri("/protected")
            .insert_header(("Authorization", "Basic dXNlcjpwYXNz"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn invalid_token_returns_401() {
        let app = test::init_service(app_with_auth()).await;
        let req = test::TestRequest::get()
            .uri("/protected")
            .insert_header(("Authorization", "Bearer not.a.real.jwt"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn expired_token_returns_401() {
        // ttl=0 → immediately expired (iat == exp, leeway=0)
        let token = auth_svc::issue_jwt(SECRET, "player-uuid", 0).expect("issue");
        // Give the token 1s to expire.
        std::thread::sleep(std::time::Duration::from_secs(1));

        let app = test::init_service(app_with_auth()).await;
        let req = test::TestRequest::get()
            .uri("/protected")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn valid_token_passes_through_with_player_id() {
        let player_id = "00000000-0000-4000-8000-000000000042";
        let token = auth_svc::issue_jwt(SECRET, player_id, 60).expect("issue");

        let app = test::init_service(app_with_auth()).await;
        let req = test::TestRequest::get()
            .uri("/protected")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["player_id"], player_id);
    }

    #[actix_web::test]
    async fn wrong_secret_returns_401() {
        // Token signed with a different secret.
        let token = auth_svc::issue_jwt("other-secret-other-secret-other-secret-xx", "pid", 60)
            .expect("issue");

        let app = test::init_service(app_with_auth()).await;
        let req = test::TestRequest::get()
            .uri("/protected")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn empty_bearer_value_returns_401() {
        let app = test::init_service(app_with_auth()).await;
        let req = test::TestRequest::get()
            .uri("/protected")
            .insert_header(("Authorization", "Bearer "))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
