//! REST API for headless control of wallhack nodes.
//!
//! Thin HTTP wrapper over the existing control plane protocol.
//! Provides endpoints for health checks, metrics, and real-time events.
//!
//! **Maintenance note:** the `OpenAPI` spec is manually maintained at
//! `website/src/data/openapi.json`. If you add, remove, or change any route,
//! request body, or response shape in this module or `handlers.rs`, update
//! that file to match.

mod auth;
mod handlers;
mod state;
mod validation;

use std::net::SocketAddr;

use axum::{
    Router,
    body::Body,
    http::{HeaderValue, Request, header},
    middleware::{self, Next},
    response::Response,
    routing::{delete, get},
};

pub use auth::Auth;
pub use state::{CorsPolicy, State};

/// Security middleware that adds protective headers.
async fn security_middleware(req: Request<Body>, next: Next) -> Response {
    let mut response = next.run(req).await;
    let headers = response.headers_mut();

    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        "Permissions-Policy",
        HeaderValue::from_static("geolocation=(), camera=(), microphone=()"),
    );

    response
}

/// Create the API router.
pub fn router(state: State) -> Router {
    let auth = state.auth.clone();

    let protected_routes = Router::new()
        .route("/status", get(handlers::status))
        .route("/stats", get(handlers::stats))
        .route("/peers", get(handlers::peers))
        .route("/peers/{name}", delete(handlers::disconnect_peer))
        .route(
            "/routes",
            get(handlers::list_routes).post(handlers::add_route),
        )
        .route("/routes/{cidr}", delete(handlers::delete_route))
        .route("/events", get(handlers::events))
        .layer(middleware::from_fn(move |req, next| {
            let auth = auth.clone();
            auth::middleware(auth, req, next)
        }));

    let cors = state.cors.clone().into_layer();

    // Health endpoint is always public, security headers apply to all
    Router::new()
        .route("/health", get(handlers::health))
        .merge(protected_routes)
        .fallback(|req: Request<Body>| async move {
            tracing::warn!(
                method = %req.method(),
                uri = %req.uri(),
                "API 404: no route matched"
            );
            axum::http::StatusCode::NOT_FOUND
        })
        .layer(cors)
        .layer(middleware::from_fn(security_middleware))
        .with_state(state)
}

/// Start the API server on the given address with HTTPS.
///
/// Uses the same TLS configuration as the main tunnel server.
/// If no TLS config is provided, generates a self-signed certificate.
///
/// # Errors
///
/// Returns an error if the server fails to bind or run.
pub async fn serve(
    addr: SocketAddr,
    state: State,
    tls_config: Option<wallhack_core::server::config::TlsConfig>,
) -> std::io::Result<()> {
    use axum_server::tls_rustls::RustlsConfig;

    // Warn if auth is not configured
    if !state.auth.is_configured() {
        tracing::warn!(
            "REST API started without authentication. \
             Configure API credentials to secure the API."
        );
    }

    let (certs, key, fingerprint) =
        wallhack_core::server::tls::configure_crypto(tls_config).map_err(std::io::Error::other)?;

    let rustls_config = RustlsConfig::from_der(
        certs.into_iter().map(|c| c.to_vec()).collect(),
        key.secret_der().to_vec(),
    )
    .await
    .map_err(std::io::Error::other)?;

    tracing::info!("REST API listening on https://{addr}");
    tracing::info!("REST API certificate: {fingerprint}");
    axum_server::bind_rustls(addr, rustls_config)
        .serve(router(state).into_make_service())
        .await
}
