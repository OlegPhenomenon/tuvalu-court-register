//! HTTP API composition. Each module exposes `pub fn routes() -> Router<AppState>` with paths
//! relative to `/api`. JSON in/out; errors via `AppError`.

pub mod admin;
pub mod audit_log;
pub mod auth;
pub mod common;
pub mod cases;
pub mod decisions;
pub mod demo;
pub mod dispatch;
pub mod documents;
pub mod export;
pub mod hearings;
pub mod import;
pub mod intake;
pub mod mailbox;
pub mod parties;
pub mod queue;
pub mod reference;
pub mod reports;
pub mod search;
pub mod tasks;

use crate::error::AppError;
use crate::state::AppState;
use axum::Router;
use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

#[derive(rust_embed::Embed)]
#[folder = "web/dist"]
struct Assets;

pub fn router(state: AppState) -> Router {
    let upload_limit = state.cfg.upload_max_bytes as usize + 1024 * 1024;
    let api = Router::new()
        .route("/health", get(|| async { axum::Json(serde_json::json!({ "ok": true })) }))
        .merge(auth::routes())
        .merge(demo::routes())
        .merge(queue::routes())
        .merge(reference::routes())
        .merge(intake::routes())
        .merge(cases::routes())
        .merge(parties::routes())
        .merge(hearings::routes())
        .merge(tasks::routes())
        .merge(documents::routes())
        .merge(decisions::routes())
        .merge(dispatch::routes())
        .merge(mailbox::routes())
        .merge(reports::routes())
        .merge(search::routes())
        .merge(import::routes())
        .merge(export::routes())
        .merge(admin::routes())
        .merge(audit_log::routes())
        .fallback(|| async { AppError::not_found() })
        .layer(axum::middleware::from_fn_with_state(state.clone(), crate::auth::password_gate))
        .layer(DefaultBodyLimit::max(upload_limit))
        .layer(axum::middleware::map_response(no_store));

    Router::new().nest("/api", api).fallback(static_handler).with_state(state)
}

/// Private data must never be stored by shared caches or the browser's disk cache.
async fn no_store(mut res: Response) -> Response {
    res.headers_mut()
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store, private"));
    res.headers_mut().insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    res
}

fn security_headers(res: &mut Response) {
    let h = res.headers_mut();
    h.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
    );
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
}

/// Serve the embedded SPA. Unknown non-asset paths get `index.html` (client-side routing).
async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let (file, path) = match Assets::get(path) {
        Some(f) if !path.is_empty() => (f, path),
        _ => match Assets::get("index.html") {
            Some(f) => (f, "index.html"),
            None => return (StatusCode::NOT_FOUND, "Frontend not built. Run `npm run build` in web/.").into_response(),
        },
    };
    let mime = match path.rsplit_once('.').map(|(_, e)| e) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    };
    let mut res = Response::new(Body::from(file.data.into_owned()));
    res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    security_headers(&mut res);
    res
}
