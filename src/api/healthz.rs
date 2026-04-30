use actix_web::{HttpResponse, web};
use sqlx::MySqlPool;
use std::time::Duration;

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(web::resource("/healthz").route(web::get().to(healthz)));
}

/// `/healthz` handler.
///
/// Always returns HTTP 200 (so an unreachable database doesn't take down a
/// load-balancer's idea of "alive"); the body surfaces the DB ping result so
/// operators can distinguish "alive but degraded" from "fully healthy".
async fn healthz(pool: Option<web::Data<MySqlPool>>) -> HttpResponse {
    let db_status = match pool {
        Some(pool) => match check_db(pool.get_ref()).await {
            Ok(()) => "ok",
            Err(_) => "down",
        },
        None => "unconfigured",
    };

    HttpResponse::Ok().json(serde_json::json!({
        "status": "ok",
        "db": db_status,
    }))
}

/// Run a `SELECT 1` against the pool with a 500ms timeout.
async fn check_db(pool: &MySqlPool) -> Result<(), ()> {
    let fut = sqlx::query_scalar::<_, i64>("SELECT 1").fetch_one(pool);
    match tokio::time::timeout(Duration::from_millis(500), fut).await {
        Ok(Ok(_)) => Ok(()),
        _ => Err(()),
    }
}

#[cfg(test)]
mod healthz_tests {
    use super::*;
    use actix_web::{App, test};

    #[actix_web::test]
    async fn test_healthz_returns_ok_without_pool() {
        let app = test::init_service(App::new().configure(config)).await;
        let req = test::TestRequest::get().uri("/healthz").to_request();
        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["status"], "ok");
        assert_eq!(body["db"], "unconfigured");
    }
}
