//! `GET /metrics` — Prometheus text-format exposition (task 0.4.9).
//!
//! Serves the process-global registry (request counters/histograms, WS gauge)
//! plus live DB pool gauges derived from the `sqlx` pool when one is present.
//! Not auth-guarded and never rate-limited — Prometheus scrapes must always
//! succeed even under load.

use crate::middleware::metrics::metrics;
use actix_web::{HttpResponse, web};
use prometheus::{Encoder, IntGauge, Opts, TextEncoder, core::Collector};
use sqlx::MySqlPool;

/// Register the `/metrics` route.
pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(web::resource("/metrics").route(web::get().to(metrics_handler)));
}

/// Render the metrics registry plus live DB pool gauges.
async fn metrics_handler(pool: Option<web::Data<MySqlPool>>) -> HttpResponse {
    let mut out = metrics().render();
    if let Some(pool) = pool {
        out.push_str(&render_pool_gauges(pool.get_ref()));
    }
    HttpResponse::Ok()
        .content_type("text/plain; version=0.0.4; charset=utf-8")
        .body(out)
}

/// Build `playzoid_db_pool_connections{state="..."}` gauges from a live pool.
///
/// Values are sampled at scrape time (no static collectors) so a pool that
/// comes up or dies after boot still reports correctly.
fn render_pool_gauges(pool: &MySqlPool) -> String {
    let size = pool.size() as usize;
    let idle = pool.num_idle();
    let in_use = size.saturating_sub(idle);
    let mut out = String::new();
    let encoder = TextEncoder::new();
    let mut buffer = Vec::new();
    for (state, value) in [("size", size), ("idle", idle), ("in_use", in_use)] {
        let gauge = IntGauge::with_opts(
            Opts::new(
                "playzoid_db_pool_connections",
                "Database pool connection counts",
            )
            .const_label("state", state),
        )
        .expect("static gauge opts are valid");
        gauge.set(value as i64);
        if let Err(e) = encoder.encode(&gauge.collect(), &mut buffer) {
            tracing::error!(error = %e, "metrics: pool gauge encoding failed");
            break;
        }
    }
    out.push_str(&String::from_utf8_lossy(&buffer));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{App, http::StatusCode, test as awtest};
    use prometheus::Encoder;

    #[actix_web::test]
    async fn metrics_endpoint_serves_text_and_no_pool() {
        // Record a sample so the (otherwise empty) counter family renders.
        metrics().record_request("GET", "200", 0.001);
        let app = awtest::init_service(App::new().configure(config)).await;
        let res = awtest::call_service(
            &app,
            awtest::TestRequest::get().uri("/metrics").to_request(),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = awtest::read_body(res).await;
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("playzoid_http_requests_total"));
        assert!(text.contains("playzoid_http_request_duration_seconds_count"));
        assert!(text.contains("playzoid_ws_connections"));
        // Without a pool no DB gauges are emitted.
        assert!(!text.contains("playzoid_db_pool_connections"));
    }

    #[test]
    fn pool_gauge_rendering_is_well_formed() {
        // Cannot build a live MySqlPool without a server, so exercise the
        // shape the handler depends on: IntGauge const labels render with the
        // expected state label.
        let gauge = IntGauge::with_opts(
            Opts::new("playzoid_db_pool_connections", "db pool").const_label("state", "size"),
        )
        .expect("opts valid");
        gauge.set(10);
        let mut buf = Vec::new();
        TextEncoder::new()
            .encode(&gauge.collect(), &mut buf)
            .expect("encodes");
        let out = String::from_utf8_lossy(&buf);
        assert!(out.contains("playzoid_db_pool_connections{state=\"size\"} 10"));
    }
}
