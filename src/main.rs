// clsi-rs: minimal CLSI HTTP server.
//
// Speaks the same JSON protocol the Overleaf web service expects from CLSI:
//
//   POST /project/:pid[/user/:uid]/compile        — main compile entrypoint
//   DELETE /project/:pid[/user/:uid]              — clear cache for project
//   GET /project/:pid/status                      — per-project ping
//   GET /health_check, /status                    — liveness
//
// What we deliberately skip vs upstream CE:
//   - sync/code, sync/pdf (SyncTeX) — not required for compile
//   - wordcount — separate endpoint, web only hits it on button click
//   - compile/stop — best-effort cancel, not load-bearing
//   - clsi-cache, content-cache, ranges/contentId — CE-internal optimizations
//
// What we do that CE doesn't:
//   - Upload outputs to R2 so a forked web can presign them and skip proxying
//     bytes through the always-on VM.

use axum::{
    extract::{Path as AxumPath, State},
    http::{HeaderMap, Request, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::{delete, get, post},
    Router,
};
use std::{net::SocketAddr, sync::Arc};
use tracing::info;

mod auth;
mod compile;
mod errors;
mod lock;
mod r2;
mod types;

pub struct AppState {
    pub work_root: std::path::PathBuf,
    pub auth_token: Option<String>,
    pub r2: r2::R2Client,
    pub locks: lock::ScopeLocks,
    pub output_url_base: String, // used to build outputFiles[].url paths
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,clsi_rs=debug".into()),
        )
        .with_target(false)
        .compact()
        .init();

    let work_root = std::env::var("CLSI_WORK_DIR")
        .unwrap_or_else(|_| "/work".into())
        .into();
    let auth_token = std::env::var("CLSI_SHARED_AUTH").ok();
    if auth_token.is_none() {
        tracing::warn!("CLSI_SHARED_AUTH not set — server is UNAUTHENTICATED");
    }

    let r2 = r2::R2Client::from_env().await?;
    // Web does `new URL(file.url).pathname` on every outputFiles[].url — it
    // needs an absolute URL. The host doesn't matter (web throws it away) but
    // it has to parse. Default to a synthetic value so deploys without an
    // explicit CLSI_OUTPUT_URL_BASE don't break.
    let output_url_base =
        std::env::var("CLSI_OUTPUT_URL_BASE").unwrap_or_else(|_| "http://clsi".into());

    let state = Arc::new(AppState {
        work_root,
        auth_token,
        r2,
        locks: lock::ScopeLocks::default(),
        output_url_base,
    });

    tokio::fs::create_dir_all(&state.work_root).await?;

    // Routes that bypass auth (liveness probes).
    let public = Router::new()
        .route("/health_check", get(health))
        .route("/status", get(health));

    // Authenticated routes. Auth runs as middleware so a missing/wrong bearer
    // returns 401 *before* we try to deserialize the JSON body — this matters
    // on platforms where we're directly exposed to public bot traffic.
    let private = Router::new()
        .route("/project/:pid/status", get(project_status))
        .route("/project/:pid/compile", post(compile::compile_no_user))
        .route(
            "/project/:pid/user/:uid/compile",
            post(compile::compile_with_user),
        )
        .route("/project/:pid", delete(clear_project_no_user))
        .route("/project/:pid/user/:uid", delete(clear_project_with_user))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    let app = public.merge(private).with_state(state.clone());

    let bind = std::env::var("CLSI_BIND").unwrap_or_else(|_| "0.0.0.0:3013".into());
    let addr: SocketAddr = bind.parse()?;
    info!("clsi-rs listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> &'static str {
    "OK"
}

async fn project_status(AxumPath(_pid): AxumPath<String>) -> &'static str {
    "OK"
}

async fn auth_middleware(
    State(s): State<Arc<AppState>>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    auth::check(&s, req.headers()).map_err(|_| StatusCode::UNAUTHORIZED)?;
    Ok(next.run(req).await)
}

async fn clear_project_no_user(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(pid): AxumPath<String>,
) -> Result<StatusCode, errors::ApiError> {
    auth::check(&s, &headers)?;
    let pid = types::validate_project_id(&pid)?;
    clear_scope(&s, &pid).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn clear_project_with_user(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath((pid, uid)): AxumPath<(String, String)>,
) -> Result<StatusCode, errors::ApiError> {
    auth::check(&s, &headers)?;
    let pid = types::validate_project_id(&pid)?;
    let uid = types::validate_user_id(&uid)?;
    let scope = format!("{pid}-{uid}");
    clear_scope(&s, &scope).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn clear_scope(s: &AppState, scope: &str) -> anyhow::Result<()> {
    let dir = s.work_root.join(scope);
    if dir.exists() {
        tokio::fs::remove_dir_all(&dir).await?;
    }
    s.r2.delete_prefix(&format!("project/{scope}/")).await?;
    Ok(())
}
