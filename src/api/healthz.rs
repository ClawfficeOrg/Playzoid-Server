use actix_web::{HttpResponse, web};

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(web::resource("/healthz").route(web::get().to(healthz)));
}

async fn healthz() -> HttpResponse {
    HttpResponse::Ok().json(serde_json::json!({"status": "ok"}))
}

#[cfg(test)]
mod healthz_tests {
    use super::*;
    use actix_web::{App, test};

    #[actix_web::test]
    async fn test_healthz_returns_ok() {
        let app = test::init_service(App::new().configure(config)).await;
        let req = test::TestRequest::get().uri("/healthz").to_request();
        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());
    }
}
