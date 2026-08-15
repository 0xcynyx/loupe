//! Wiring only: config, shared state, routes, embedded static assets, listener.

mod api;
mod config;
mod milvus;

use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;
use tokio::sync::RwLock;

async fn index() -> impl IntoResponse {
    ([(CONTENT_TYPE, "text/html; charset=utf-8")], include_str!("../web/index.html"))
}

async fn app_js() -> impl IntoResponse {
    ([(CONTENT_TYPE, "application/javascript")], include_str!("../web/app.js"))
}

async fn style_css() -> impl IntoResponse {
    ([(CONTENT_TYPE, "text/css")], include_str!("../web/style.css"))
}

async fn favicon() -> impl IntoResponse {
    ([(CONTENT_TYPE, "image/svg+xml")], include_str!("../web/favicon.svg"))
}

#[tokio::main]
async fn main() {
    let config = config::Config::load();
    let port = config.gui_port;
    let bind = config.bind.clone();
    let state = api::AppState {
        config,
        sessions: Arc::new(RwLock::new(std::collections::HashMap::new())),
        sort_cache: Arc::new(RwLock::new(std::collections::HashMap::new())),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/style.css", get(style_css))
        .route("/favicon.svg", get(favicon))
        .route("/api/defaults", get(api::defaults))
        .route("/api/connect", post(api::connect))
        .route("/api/disconnect", post(api::disconnect))
        .route("/api/collections", get(api::list_collections))
        .route("/api/collections/{name}", get(api::describe_collection))
        .route("/api/collections/{name}/query", post(api::query_collection))
        .route("/api/collections/{name}/load", post(api::load_collection))
        .route("/api/collections/{name}/release", post(api::release_collection))
        .with_state(state);

    let addr = format!("{bind}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind failed");
    println!("loupe running at http://{addr}");
    axum::serve(listener, app).await.expect("server failed");
}
