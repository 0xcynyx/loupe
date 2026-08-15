//! HTTP handlers mapping the GUI API onto the MilvusApi trait, multi session aware.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::future::join_all;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

use crate::config::Config;
use crate::milvus::{MilvusApi, RestClient};

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);

const SORT_BATCH: usize = 10_000;
const SORT_CACHE_MAX: usize = 8;

// Sorted primary keys for one (collection, filter, field, dir), pages are served by pk lookup.
pub struct SortIndex {
    pk_field: String,
    keys: Vec<Value>,
}

#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub sessions: Arc<RwLock<HashMap<String, Arc<dyn MilvusApi>>>>,
    pub sort_cache: Arc<RwLock<HashMap<String, Arc<SortIndex>>>>,
}

type ApiResult = std::result::Result<Json<Value>, ApiError>;

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

impl<E: std::fmt::Display> From<E> for ApiError {
    fn from(err: E) -> Self {
        ApiError(StatusCode::BAD_GATEWAY, err.to_string())
    }
}

fn new_session_id() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{:x}-{:x}", nanos, SESSION_COUNTER.fetch_add(1, Ordering::Relaxed))
}

// Resolves the caller's session from the X-Session header without holding the lock across awaits.
async fn client(state: &AppState, headers: &HeaderMap) -> std::result::Result<Arc<dyn MilvusApi>, ApiError> {
    let id = headers
        .get("x-session")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "missing X-Session header".to_string()))?;
    state
        .sessions
        .read()
        .await
        .get(id)
        .cloned()
        .ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "session expired, sign in again".to_string()))
}

pub async fn defaults(State(state): State<AppState>) -> Json<Value> {
    let c = &state.config;
    Json(json!({"host": c.host, "port": c.port, "user": c.user, "password": c.password}))
}

#[derive(Deserialize)]
pub struct ConnectBody {
    host: String,
    #[serde(default = "default_port")]
    port: String,
    user: String,
    password: String,
}

fn default_port() -> String {
    "19530".to_string()
}

pub async fn connect(State(state): State<AppState>, Json(body): Json<ConnectBody>) -> ApiResult {
    let client = RestClient::new(&body.host, &body.port, &body.user, &body.password)?;
    client.list_collections().await?;
    let id = new_session_id();
    state.sessions.write().await.insert(id.clone(), Arc::new(client));
    Ok(Json(json!({"session": id})))
}

pub async fn disconnect(State(state): State<AppState>, headers: HeaderMap) -> Json<Value> {
    if let Some(id) = headers.get("x-session").and_then(|v| v.to_str().ok()) {
        state.sessions.write().await.remove(id);
    }
    Json(json!({"ok": true}))
}

pub async fn list_collections(State(state): State<AppState>, headers: HeaderMap) -> ApiResult {
    let client = client(&state, &headers).await?;
    let mut names = client.list_collections().await?;
    names.sort();
    let summaries = join_all(names.iter().map(|name| summarize(client.clone(), name.clone()))).await;
    Ok(Json(Value::Array(summaries)))
}

// Row count and load state fetched concurrently per collection, errors degrade to placeholders.
async fn summarize(client: Arc<dyn MilvusApi>, name: String) -> Value {
    let (rows, loaded) = tokio::join!(client.row_count(&name), client.load_state(&name));
    json!({
        "name": name,
        "rows": rows.unwrap_or(-1),
        "loaded": loaded.unwrap_or_else(|_| "unknown".to_string()),
    })
}

pub async fn describe_collection(State(state): State<AppState>, Path(name): Path<String>, headers: HeaderMap) -> ApiResult {
    let client = client(&state, &headers).await?;
    let (describe, rows, loaded) = tokio::join!(client.describe(&name), client.row_count(&name), client.load_state(&name));
    let describe = describe?;
    Ok(Json(json!({
        "name": name,
        "description": describe["description"].as_str().unwrap_or(""),
        "rows": rows.unwrap_or(-1),
        "loaded": loaded.unwrap_or_else(|_| "unknown".to_string()),
        "fields": describe["fields"],
        "indexes": describe["indexes"],
    })))
}

#[derive(Deserialize)]
pub struct QueryBody {
    #[serde(default)]
    filter: String,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    sort_field: String,
    #[serde(default)]
    sort_dir: String,
    #[serde(default)]
    refresh: bool,
}

fn default_limit() -> usize {
    50
}

pub async fn query_collection(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(body): Json<QueryBody>,
) -> ApiResult {
    let client = client(&state, &headers).await?;
    let describe = client.describe(&name).await?;
    let fields = scalar_fields(&describe);
    if fields.is_empty() {
        return Err(ApiError(StatusCode::UNPROCESSABLE_ENTITY, "collection has no scalar fields to display".to_string()));
    }
    let limit = body.limit.clamp(1, state.config.row_cap);
    let total = client.count(&name, &body.filter).await?;

    if body.sort_field.is_empty() {
        let rows = client.query(&name, &body.filter, &fields, limit, body.offset).await?;
        return Ok(Json(json!({"fields": fields, "rows": rows, "limit": limit, "offset": body.offset, "total": total})));
    }

    if !fields.contains(&body.sort_field) {
        return Err(ApiError(StatusCode::UNPROCESSABLE_ENTITY, format!("unknown sort field: {}", body.sort_field)));
    }
    if total as usize > state.config.sort_cap {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("{total} rows exceed the whole collection sort cap of {}, raise MILVUS_GUI_SORT_CAP or add a filter", state.config.sort_cap),
        ));
    }

    let session_id = headers.get("x-session").and_then(|v| v.to_str().ok()).unwrap_or("");
    let cache_key = format!("{session_id}|{name}|{}|{}|{}", body.filter, body.sort_field, body.sort_dir);
    let index = match cached_index(&state, &cache_key, body.refresh).await {
        Some(index) => index,
        None => build_sort_index(&state, client.clone(), &describe, &name, &body, &cache_key).await?,
    };

    let page_keys: Vec<Value> = index.keys.iter().skip(body.offset).take(limit).cloned().collect();
    let rows = fetch_rows_by_pk(client, &name, &index.pk_field, &page_keys, &fields).await?;
    Ok(Json(json!({"fields": fields, "rows": rows, "limit": limit, "offset": body.offset, "total": total, "sorted": true})))
}

async fn cached_index(state: &AppState, key: &str, refresh: bool) -> Option<Arc<SortIndex>> {
    if refresh {
        state.sort_cache.write().await.remove(key);
        return None;
    }
    state.sort_cache.read().await.get(key).cloned()
}

// Streams (pk, sort key) pairs via pk keyset batches, sorts them once, caches the pk order.
async fn build_sort_index(
    state: &AppState,
    client: Arc<dyn MilvusApi>,
    describe: &Value,
    name: &str,
    body: &QueryBody,
    cache_key: &str,
) -> std::result::Result<Arc<SortIndex>, ApiError> {
    let (pk_field, pk_is_string) = primary_key(describe)
        .ok_or_else(|| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "collection has no primary key".to_string()))?;
    let wanted = vec![pk_field.clone(), body.sort_field.clone()];

    let mut pairs: Vec<(Value, Value)> = Vec::new();
    let mut last_pk: Option<Value> = None;
    loop {
        let keyset = last_pk.as_ref().map(|pk| format!("{} > {}", pk_field, pk_literal(pk, pk_is_string)));
        let expr = match (&body.filter, keyset) {
            (f, Some(k)) if !f.is_empty() => format!("({f}) and {k}"),
            (_, Some(k)) => k,
            (f, None) => f.clone(),
        };
        let batch = client.query(name, &expr, &wanted, SORT_BATCH, 0).await?;
        let done = batch.len() < SORT_BATCH;
        for row in &batch {
            pairs.push((row[&pk_field].clone(), row[&body.sort_field].clone()));
        }
        if let Some(row) = batch.last() {
            last_pk = Some(row[&pk_field].clone());
        }
        if done {
            break;
        }
        if pairs.len() > state.config.sort_cap {
            return Err(ApiError(StatusCode::UNPROCESSABLE_ENTITY, "collection grew past the sort cap while indexing".to_string()));
        }
    }

    let descending = body.sort_dir == "desc";
    pairs.sort_by(|a, b| {
        let ord = compare_values(&a.1, &b.1);
        if descending { ord.reverse() } else { ord }
    });

    let index = Arc::new(SortIndex {
        pk_field,
        keys: pairs.into_iter().map(|(pk, _)| pk).collect(),
    });
    let mut cache = state.sort_cache.write().await;
    if cache.len() >= SORT_CACHE_MAX {
        cache.clear();
    }
    cache.insert(cache_key.to_string(), index.clone());
    Ok(index)
}

// Fetches full rows for one page of pks and restores the sorted order.
async fn fetch_rows_by_pk(
    client: Arc<dyn MilvusApi>,
    name: &str,
    pk_field: &str,
    page_keys: &[Value],
    fields: &[String],
) -> std::result::Result<Vec<Value>, ApiError> {
    if page_keys.is_empty() {
        return Ok(Vec::new());
    }
    let pk_is_string = page_keys[0].is_string();
    let literals: Vec<String> = page_keys.iter().map(|pk| pk_literal(pk, pk_is_string)).collect();
    let expr = format!("{} in [{}]", pk_field, literals.join(", "));
    let rows = client.query(name, &expr, &fields.to_vec(), page_keys.len(), 0).await?;
    let mut by_pk: HashMap<String, Value> = rows.into_iter().map(|r| (r[pk_field].to_string(), r)).collect();
    Ok(page_keys.iter().filter_map(|pk| by_pk.remove(&pk.to_string())).collect())
}

fn primary_key(describe: &Value) -> Option<(String, bool)> {
    describe["fields"].as_array()?.iter().find(|f| f["primaryKey"].as_bool() == Some(true)).map(|f| {
        (
            f["name"].as_str().unwrap_or("id").to_string(),
            f["type"].as_str().unwrap_or("") == "VarChar",
        )
    })
}

fn pk_literal(pk: &Value, is_string: bool) -> String {
    if is_string {
        serde_json::to_string(pk.as_str().unwrap_or_default()).unwrap_or_else(|_| "\"\"".to_string())
    } else {
        pk.to_string()
    }
}

// Total order over JSON values: numbers, then strings, then bools, nulls always last.
fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    fn rank(v: &Value) -> u8 {
        match v {
            Value::Number(_) => 0,
            Value::String(_) => 1,
            Value::Bool(_) => 2,
            Value::Null => 4,
            _ => 3,
        }
    }
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x
            .as_f64()
            .unwrap_or(f64::NAN)
            .partial_cmp(&y.as_f64().unwrap_or(f64::NAN))
            .unwrap_or(Ordering::Equal),
        (Value::String(x), Value::String(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ if rank(a) != rank(b) => rank(a).cmp(&rank(b)),
        _ => a.to_string().cmp(&b.to_string()),
    }
}

// Vector fields are excluded from row output, the grid shows scalars only.
fn scalar_fields(describe: &Value) -> Vec<String> {
    describe["fields"]
        .as_array()
        .map(|fields| {
            fields
                .iter()
                .filter(|f| !f["type"].as_str().unwrap_or("").ends_with("Vector"))
                .filter_map(|f| f["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

pub async fn load_collection(State(state): State<AppState>, Path(name): Path<String>, headers: HeaderMap) -> ApiResult {
    client(&state, &headers).await?.load(&name).await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn release_collection(State(state): State<AppState>, Path(name): Path<String>, headers: HeaderMap) -> ApiResult {
    client(&state, &headers).await?.release(&name).await?;
    Ok(Json(json!({"ok": true})))
}
