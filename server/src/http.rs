use std::path::PathBuf;

use anyhow::{Context, bail};
use axum::{
    Router,
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get},
};
use sqlx::PgPool;
use tower_http::services::{ServeDir, ServeFile};

use crate::{
    access::{self, Access},
    artifacts, auth, browse, delivery, device,
    error::ApiError,
    retrieval, sync,
};

/// The web reader, protected by Cloudflare Access.
pub struct Reader {
    pub access: Access,
    pub assets: PathBuf,
}

/// Machines use `/v1/*`, with source tokens except to log in. Without a
/// `reader`, nothing else is served. `read` serves archive reads and should be
/// a read-only pool.
pub fn router(pool: PgPool, read: PgPool, reader: Option<Reader>) -> anyhow::Result<Router> {
    let mailbox = delivery::Mailbox::new(pool.clone());
    let machine = Router::new()
        .merge(sync::machine_routes(pool.clone()))
        .merge(retrieval::machine_routes().with_state(read.clone()))
        .merge(artifacts::machine_routes(pool.clone(), read.clone()))
        .merge(delivery::machine_routes(mailbox.clone()))
        .method_not_allowed_fallback(method_not_allowed)
        .route_layer(middleware::from_fn_with_state(pool.clone(), auth::require))
        .merge(device::machine_routes().with_state(pool.clone()));
    let app = Router::new()
        .route("/healthz", get(health))
        .with_state(pool.clone())
        .merge(machine)
        .route("/v1", any(not_found))
        .route("/v1/{*rest}", any(not_found))
        .method_not_allowed_fallback(method_not_allowed);
    let Some(Reader { access, assets }) = reader else {
        return Ok(app.fallback(not_found));
    };
    let index = assets.join("index.html");
    let metadata = std::fs::metadata(&index).with_context(|| {
        format!(
            "web assets missing: {} is required; build the web frontend first or set ASSETS_DIR",
            index.display()
        )
    })?;
    if !metadata.is_file() {
        bail!("web assets invalid: {} must be a file", index.display());
    }
    // Archive reads use the read-only pool; device approval and sending messages write.
    let reader = Router::new()
        .merge(browse::reader_routes())
        .merge(artifacts::reader_routes())
        .with_state(read)
        .merge(device::reader_routes().with_state(pool))
        .merge(delivery::reader_routes().with_state(mailbox))
        .route_service("/", ServeFile::new(index.clone()))
        .route_service("/sessions/{*reference}", ServeFile::new(index.clone()))
        .route_service("/device", ServeFile::new(index.clone()))
        .route_service("/artifacts", ServeFile::new(index.clone()))
        .route_service("/artifacts/{id}", ServeFile::new(index.clone()))
        .route_service("/index.html", ServeFile::new(index))
        .nest_service("/assets", ServeDir::new(assets.join("assets")))
        .route_service("/favicon.ico", ServeFile::new(assets.join("favicon.ico")))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::from_fn_with_state(access, access::require))
        .layer(middleware::from_fn(privacy));
    Ok(app.fallback_service(reader))
}

async fn not_found() -> ApiError {
    ApiError::not_found()
}

async fn method_not_allowed() -> ApiError {
    ApiError::new(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed")
}

async fn health(State(pool): State<PgPool>) -> StatusCode {
    match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&pool)
        .await
    {
        Ok(_) => StatusCode::OK,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

async fn privacy(request: Request, next: Next) -> Response {
    let mut response = if trusted_request(&request) {
        next.run(request).await
    } else {
        ApiError::new(StatusCode::FORBIDDEN, "forbidden_origin").into_response()
    };
    // Artifact content sets its own framing and content policy.
    let headers = response.headers_mut();
    for (name, value) in [
        ("cache-control", "no-store"),
        ("referrer-policy", "no-referrer"),
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("cross-origin-resource-policy", "same-origin"),
        ("strict-transport-security", "max-age=31536000"),
        (
            "content-security-policy",
            "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src * data: blob:; font-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'; frame-src 'self'",
        ),
    ] {
        headers
            .entry(name)
            .or_insert(HeaderValue::from_static(value));
    }
    response
}

/// The API serves only the reader's own pages. TLS terminates before this
/// service, so an Origin is compared with the Host by authority, not scheme.
fn trusted_request(request: &Request) -> bool {
    let path = request.uri().path();
    if path != "/api" && !path.starts_with("/api/") {
        return true;
    }
    let headers = request.headers();
    if headers.get_all("sec-fetch-site").iter().any(|value| {
        !value
            .to_str()
            .is_ok_and(|value| matches!(value, "same-origin" | "none"))
    }) {
        return false;
    }
    let mut origins = headers.get_all("origin").iter().peekable();
    if origins.peek().is_none() {
        return true;
    }
    let mut hosts = headers.get_all("host").iter();
    let (Some(host), None) = (hosts.next(), hosts.next()) else {
        return false;
    };
    origins.all(|origin| {
        origin
            .to_str()
            .ok()
            .and_then(|origin| origin.split_once("://"))
            .is_some_and(|(scheme, authority)| {
                matches!(scheme, "http" | "https") && authority.as_bytes() == host.as_bytes()
            })
    })
}
