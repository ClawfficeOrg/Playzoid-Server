//! Prometheus metrics: per-request counters/histograms and a WebSocket
//! connection gauge (task 0.4.9).
//!
//! A process-global [`Metrics`] registry is shared by the request middleware,
//! the WebSocket gauge hooks in `src/sockets/ws.rs`, and the `/metrics`
//! handler (see [`crate::api::metrics`]). DB pool stats are rendered live by
//! the handler rather than held as static collectors.
//!
//! Registered via `.wrap(MetricsMiddleware)` — outermost, so it measures the
//! whole request pipeline including rate-limit decisions.

use actix_web::{
    Error,
    body::MessageBody,
    dev::{Service, ServiceRequest, ServiceResponse, Transform, forward_ready},
};
use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, IntGauge, Opts, Registry, TextEncoder,
};
use std::{
    future::{Future, Ready, ready},
    pin::Pin,
    sync::LazyLock,
    time::Instant,
};

/// HTTP latency histogram buckets in seconds — spans fast health probes to
/// slow SQL-bound handlers.
const HTTP_DURATION_BUCKETS: &[f64] = &[
    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Process-wide metric registry and collectors.
pub struct Metrics {
    registry: Registry,
    http_requests: IntCounterVec,
    http_duration: HistogramVec,
    ws_connections: IntGauge,
}

impl Metrics {
    /// Create a fresh registry with the standard collectors registered.
    pub fn new() -> Result<Self, prometheus::Error> {
        let registry = Registry::new();
        let http_requests = IntCounterVec::new(
            Opts::new("playzoid_http_requests_total", "HTTP requests handled"),
            &["method", "status"],
        )?;
        let http_duration = HistogramVec::new(
            HistogramOpts::new(
                "playzoid_http_request_duration_seconds",
                "HTTP request latency",
            )
            .buckets(HTTP_DURATION_BUCKETS.to_vec()),
            &["method", "status"],
        )?;
        let ws_connections = IntGauge::new(
            "playzoid_ws_connections",
            "Current number of active WebSocket connections",
        )?;
        registry.register(Box::new(http_requests.clone()))?;
        registry.register(Box::new(http_duration.clone()))?;
        registry.register(Box::new(ws_connections.clone()))?;
        Ok(Self {
            registry,
            http_requests,
            http_duration,
            ws_connections,
        })
    }

    /// Record one completed HTTP request by method and status code.
    pub fn record_request(&self, method: &str, status: &str, latency_secs: f64) {
        self.http_requests
            .with_label_values(&[method, status])
            .inc();
        self.http_duration
            .with_label_values(&[method, status])
            .observe(latency_secs);
    }

    /// Mark one WebSocket connection opened.
    pub fn ws_connected(&self) {
        self.ws_connections.inc();
    }

    /// Mark one WebSocket connection closed.
    pub fn ws_disconnected(&self) {
        self.ws_connections.dec();
    }

    /// Render every registered collector in Prometheus text format.
    ///
    /// Empty on encoding failure (logged); the `/metrics` handler then serves
    /// whatever it could produce plus the live DB pool gauges.
    pub fn render(&self) -> String {
        let encoder = TextEncoder::new();
        let mut buffer = Vec::new();
        if let Err(e) = encoder.encode(&self.registry.gather(), &mut buffer) {
            tracing::error!(error = %e, "metrics: text encoding failed");
            return String::new();
        }
        String::from_utf8_lossy(&buffer).into_owned()
    }
}

/// Process-global metrics singleton.
static METRICS: LazyLock<Metrics> = LazyLock::new(|| {
    Metrics::new().expect("metrics registry must initialise (static opts are valid)")
});

/// Access the process-global metrics registry.
pub fn metrics() -> &'static Metrics {
    &METRICS
}

/// Transform factory registering the per-request metrics middleware.
#[derive(Default)]
pub struct MetricsMiddleware;

impl<S, B> Transform<S, ServiceRequest> for MetricsMiddleware
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S: 'static,
    S::Future: 'static,
    B: MessageBody + 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type InitError = ();
    type Transform = MetricsMiddlewareService<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(MetricsMiddlewareService { service }))
    }
}

/// Middleware service recording one request observation per handled request.
pub struct MetricsMiddlewareService<S> {
    service: S,
}

impl<S, B> Service<ServiceRequest> for MetricsMiddlewareService<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: MessageBody + 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        // Never scrape the metrics endpoint itself — avoids self-referential
        // counters and an accidental scrap loop under the exporter.
        if req.path() == "/metrics" {
            let fut = self.service.call(req);
            return Box::pin(fut);
        }
        let method = req.method().as_str().to_string();
        let start = Instant::now();
        let fut = self.service.call(req);
        Box::pin(async move {
            let res = fut.await?;
            let status = res.status().as_u16().to_string();
            metrics().record_request(&method, &status, start.elapsed().as_secs_f64());
            Ok(res)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{App, HttpResponse, http::StatusCode, test as awtest, web};

    #[test]
    fn registry_renders_registered_collectors() {
        let m = Metrics::new().expect("registry builds");
        m.record_request("GET", "200", 0.001);
        m.ws_connected();
        let out = m.render();
        assert!(out.contains("playzoid_http_requests_total{method=\"GET\",status=\"200\"} 1"));
        assert!(out.contains(
            "playzoid_http_request_duration_seconds_count{method=\"GET\",status=\"200\"} 1"
        ));
        assert!(out.contains("playzoid_ws_connections 1"));
    }

    #[test]
    fn ws_gauge_tracks_balance() {
        let m = Metrics::new().expect("registry builds");
        m.ws_connected();
        m.ws_connected();
        m.ws_disconnected();
        assert!(m.render().contains("playzoid_ws_connections 1"));
    }

    #[actix_web::test]
    async fn middleware_records_requests_and_skips_metrics_path() {
        let app = awtest::init_service(
            App::new()
                .wrap(MetricsMiddleware)
                .route(
                    "/ping",
                    web::get().to(|| async { HttpResponse::Ok().finish() }),
                )
                .route(
                    "/metrics",
                    web::get().to(|| async { HttpResponse::Ok().body("x") }),
                ),
        )
        .await;

        let res =
            awtest::call_service(&app, awtest::TestRequest::get().uri("/ping").to_request()).await;
        assert_eq!(res.status(), StatusCode::OK);

        let before = metrics().render();
        let labels_line = "playzoid_http_requests_total{method=\"GET\",status=\"200\"}";
        assert!(before.contains(labels_line), "middleware must record ping");

        // /metrics requests are not recorded — the rendered output must not
        // change between two consecutive scrapes.
        let scrape1 = metrics().render();
        let _ = awtest::call_service(
            &app,
            awtest::TestRequest::get().uri("/metrics").to_request(),
        )
        .await;
        let scrape2 = metrics().render();
        assert_eq!(
            scrape1, scrape2,
            "the /metrics route must not record itself"
        );
    }
}
