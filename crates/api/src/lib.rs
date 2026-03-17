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
    http::{HeaderValue, Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get},
};

pub use auth::Auth;
pub use state::{CorsPolicy, State};

/// Security middleware that adds protective headers, validates requests,
/// and handles CORS (including preflight `OPTIONS`).
async fn security_middleware(
    axum::extract::State(state): axum::extract::State<State>,
    req: Request<Body>,
    next: Next,
) -> Response {
    // DNS rebinding protection: validate Host header
    let host_valid = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| validation::validate_host(h).is_ok());

    if !host_valid {
        return (StatusCode::BAD_REQUEST, "Invalid Host header").into_response();
    }

    // Extract the request Origin for CORS validation.
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|h| h.to_str().ok())
        .map(String::from);
    let allowed_origin = origin
        .as_deref()
        .filter(|o| state.cors.is_allowed(o))
        .and_then(|o| HeaderValue::from_str(o).ok());

    // Handle CORS preflight (OPTIONS) — return early with headers only.
    if req.method() == axum::http::Method::OPTIONS && allowed_origin.is_some() {
        let mut response = StatusCode::NO_CONTENT.into_response();
        let headers = response.headers_mut();
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            allowed_origin.clone().expect("checked above"),
        );
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, PUT, DELETE, OPTIONS"),
        );
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("Authorization, Content-Type"),
        );
        headers.insert(
            header::ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static("3600"),
        );
        return response;
    }

    let mut response = next.run(req).await;
    let headers = response.headers_mut();

    // CORS: reflect allowed origin if the request Origin matched the policy.
    if let Some(allowed) = allowed_origin {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, allowed);
    }

    // Content Security Policy - strict, API only serves JSON
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
    );

    // Prevent clickjacking
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));

    // Prevent MIME sniffing
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );

    // Disable caching for all API responses
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate"),
    );

    // Referrer policy
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );

    // Permissions policy - disable all features
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
        .route("/ping", get(handlers::ping))
        .route("/stats", get(handlers::stats))
        .route("/peers", get(handlers::peers))
        .route("/peers/{name}", delete(handlers::disconnect_peer))
        .route(
            "/routes",
            get(handlers::list_routes).post(handlers::add_route),
        )
        .route("/routes/{cidr}", delete(handlers::delete_route))
        .layer(middleware::from_fn(move |req, next| {
            let auth = auth.clone();
            auth::middleware(auth, req, next)
        }));

    // Health endpoint is always public, security headers apply to all
    Router::new()
        .route("/health", get(handlers::health))
        .merge(protected_routes)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security_middleware,
        ))
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

    // Use the same TLS setup as the main server
    let (certs, key, _fingerprint) =
        wallhack_core::server::tls::configure_crypto(tls_config).map_err(std::io::Error::other)?;

    let rustls_config = RustlsConfig::from_der(
        certs.into_iter().map(|c| c.to_vec()).collect(),
        key.secret_der().to_vec(),
    )
    .await
    .map_err(std::io::Error::other)?;

    tracing::info!("REST API listening on https://{addr}");
    axum_server::bind_rustls(addr, rustls_config)
        .serve(router(state).into_make_service())
        .await
}
