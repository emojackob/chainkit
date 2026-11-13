use crate::{envelope, openapi, ApiState};
use axum::{
    extract::{DefaultBodyLimit, Path, Query as HttpQuery, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chainkit_indexer::{IndexerError, Query};
use serde::Deserialize;
use serde_json::{json, Value};

pub const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/health", get(|| async { Json(json!({"status":"ok"})) }))
        .route("/openapi.json", get(|| async { Json(openapi::document()) }))
        .route(
            "/v1/projects/{project}/contracts",
            get(contracts).post(register),
        )
        .route(
            "/v1/projects/{project}/chains/{chain_id}/contracts/{address}/coverage",
            get(coverage),
        )
        .route(
            "/v1/projects/{project}/chains/{chain_id}/contracts/{address}/transactions",
            get(transactions),
        )
        .route(
            "/v1/projects/{project}/chains/{chain_id}/contracts/{address}/events",
            get(events),
        )
        .route(
            "/v1/projects/{project}/chains/{chain_id}/contracts/{address}/context/{tx_hash}",
            get(context),
        )
        .route(
            "/v1/projects/{project}/chains/{chain_id}/contracts/{address}/abis",
            get(abis).post(set_abi),
        )
        .route(
            "/v1/projects/{project}/chains/{chain_id}/contracts/{address}/pause",
            post(pause),
        )
        .route(
            "/v1/projects/{project}/chains/{chain_id}/contracts/{address}/backfill",
            post(backfill),
        )
        .fallback(|| async {
            ApiError::new(StatusCode::NOT_FOUND, "not_found", "Endpoint not found")
        })
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(axum::middleware::map_response(normalize_errors))
        .with_state(state)
}

async fn normalize_errors(response: Response) -> Response {
    if response.status().is_client_error() || response.status().is_server_error() {
        let is_json = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("application/json"));
        if !is_json {
            let status = response.status();
            return ApiError::new(
                status,
                "invalid_request",
                status.canonical_reason().unwrap_or("Request failed"),
            )
            .into_response();
        }
    }
    response
}

pub struct ApiError {
    status: StatusCode,
    code: String,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, code: &str, message: &str) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
        }
    }
}

impl From<IndexerError> for ApiError {
    fn from(error: IndexerError) -> Self {
        let category = error.category.as_str();
        let status =
            StatusCode::from_u16(error.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        Self::new(status, category, &error.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({"schema_version":"1", "error":{"code":self.code,"message":self.message}})),
        )
            .into_response()
    }
}

type ApiResult = Result<Response, ApiError>;
type Identity = (String, u64, String);

fn response(value: Value) -> ApiResult {
    let bytes = serde_json::to_vec(&envelope(value)).map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "serialization",
            "Response serialization failed",
        )
    })?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            "Response exceeds limit; use smaller pages",
        ));
    }
    Ok(([(header::CONTENT_TYPE, "application/json")], bytes).into_response())
}

fn authorize(state: &ApiState, headers: &HeaderMap, project: &str) -> Result<(), ApiError> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Project manage token required",
            )
        })?;
    state.store.authorize(project, token).map_err(Into::into)
}

async fn contracts(State(state): State<ApiState>, Path(project): Path<String>) -> ApiResult {
    response(state.store.contracts(&project)?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Register {
    chain_id: u64,
    address: String,
    start_block: u64,
    #[serde(default = "full_mode")]
    mode: String,
    abi: Option<Value>,
}
fn full_mode() -> String {
    "full".into()
}

async fn register(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    headers: HeaderMap,
    Json(input): Json<Register>,
) -> ApiResult {
    authorize(&state, &headers, &project)?;
    if !state.chain_ids.contains(&input.chain_id) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "unconfigured_chain",
            "Chain is not configured by the operator",
        ));
    }
    let (abi, source) = match input.abi {
        Some(abi) => (abi, "manual".to_string()),
        None => {
            let resolved = state.resolver.resolve(input.chain_id, &input.address).await
                .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "abi_unavailable", "Automatic ABI lookup failed; supply an ABI explicitly, or [] to collect raw evidence"))?;
            (resolved.abi, resolved.source)
        }
    };
    let registered = state.store.register_with_source(
        &project,
        input.chain_id,
        &input.address,
        abi,
        input.start_block,
        &input.mode,
        &source,
    )?;
    response(json!({"registration":registered,"abi_source":source}))
}

async fn coverage(State(state): State<ApiState>, Path((p, c, a)): Path<Identity>) -> ApiResult {
    response(state.store.coverage(&p, c, &a)?)
}
async fn transactions(
    State(state): State<ApiState>,
    Path((p, c, a)): Path<Identity>,
    HttpQuery(q): HttpQuery<Query>,
) -> ApiResult {
    response(state.store.query(&p, c, &a, "transactions", &q)?)
}
async fn events(
    State(state): State<ApiState>,
    Path((p, c, a)): Path<Identity>,
    HttpQuery(q): HttpQuery<Query>,
) -> ApiResult {
    response(state.store.query(&p, c, &a, "events", &q)?)
}
async fn context(
    State(state): State<ApiState>,
    Path((p, c, a, t)): Path<(String, u64, String, String)>,
    HttpQuery(q): HttpQuery<Query>,
) -> ApiResult {
    response(state.store.context(&p, c, &a, &t, &q)?)
}
async fn abis(State(state): State<ApiState>, Path((p, c, a)): Path<Identity>) -> ApiResult {
    response(state.store.abi_history(&p, c, &a)?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pause {
    paused: bool,
}
async fn pause(
    State(state): State<ApiState>,
    Path((p, c, a)): Path<Identity>,
    headers: HeaderMap,
    Json(input): Json<Pause>,
) -> ApiResult {
    authorize(&state, &headers, &p)?;
    response(state.store.set_paused(&p, c, &a, input.paused)?)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Backfill {
    from_block: u64,
}
async fn backfill(
    State(state): State<ApiState>,
    Path((p, c, a)): Path<Identity>,
    headers: HeaderMap,
    Json(input): Json<Backfill>,
) -> ApiResult {
    authorize(&state, &headers, &p)?;
    response(state.store.backfill(&p, c, &a, input.from_block)?)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    abi: Value,
    effective_from: u64,
}
async fn set_abi(
    State(state): State<ApiState>,
    Path((p, c, a)): Path<Identity>,
    headers: HeaderMap,
    Json(input): Json<Binding>,
) -> ApiResult {
    authorize(&state, &headers, &p)?;
    response(state.store.update_abi_with_source(
        &p,
        c,
        &a,
        input.abi,
        input.effective_from,
        "manual",
    )?)
}
