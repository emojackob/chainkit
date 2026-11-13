//! Bounded read-only JSON-RPC acquisition with per-endpoint admission control.
//!
//! Endpoints are trusted operator configuration. Provider URLs and error messages
//! never appear in public errors, debug output, or health snapshots. A pool only
//! executes an explicit allowlist of read operations; it cannot submit transactions.

use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    fmt,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime},
};
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio::time::{timeout_at, Instant as TokioInstant};

/// Pool-wide limits. Per-request timeout includes rate admission and body reads.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RpcConfig {
    pub max_attempts: u32,
    pub timeout_ms: u64,
    pub total_timeout_ms: u64,
    pub max_response_bytes: usize,
    pub base_backoff_ms: u64,
    pub max_backoff_ms: u64,
    pub cooldown_ms: u64,
    pub max_in_flight_per_endpoint: usize,
    pub requests_per_second: Option<u32>,
    pub identity_ttl_ms: u64,
    /// Pin this when the operator knows the chain genesis; otherwise the first
    /// validated endpoint establishes the expected genesis for the entire pool.
    pub expected_genesis_hash: Option<String>,
}

impl Default for RpcConfig {
    fn default() -> Self {
        Self {
            max_attempts: 6,
            timeout_ms: 15_000,
            total_timeout_ms: 60_000,
            max_response_bytes: 32 * 1024 * 1024,
            base_backoff_ms: 100,
            max_backoff_ms: 5_000,
            cooldown_ms: 1_000,
            max_in_flight_per_endpoint: 4,
            requests_per_second: None,
            identity_ttl_ms: 300_000,
            expected_genesis_hash: None,
        }
    }
}

/// Sanitized classification; provider strings and credentials are never retained.
#[derive(Clone, Debug, thiserror::Error, Serialize)]
#[error("{message}")]
pub struct RpcError {
    pub category: String,
    message: String,
    #[serde(skip)]
    retry_after: Option<Duration>,
}

impl RpcError {
    fn new(category: &str, message: &str) -> Self {
        Self {
            category: category.into(),
            message: message.into(),
            retry_after: None,
        }
    }

    pub fn category(&self) -> &str {
        &self.category
    }

    fn retryable(&self) -> bool {
        matches!(self.category.as_str(), "transient" | "timeout")
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct EndpointHealth {
    pub endpoint: String,
    pub status: String,
    pub in_flight: usize,
    pub requests: u64,
    pub successes: u64,
    pub failures: u64,
    pub consecutive_failures: u32,
    pub latency_ms: Option<u64>,
    pub unsupported_methods: Vec<String>,
}

struct Endpoint {
    url: String,
    identity_lock: AsyncMutex<()>,
    send_lock: AsyncMutex<()>,
}

#[derive(Default)]
struct EndpointState {
    in_flight: usize,
    requests: u64,
    successes: u64,
    failures: u64,
    consecutive_failures: u32,
    latency_ms: Option<u64>,
    quarantined: bool,
    cool_until: Option<Instant>,
    identity_until: Option<Instant>,
    next_request: Option<Instant>,
    unsupported: HashSet<String>,
}

struct State {
    endpoints: Vec<EndpointState>,
    cursor: usize,
    genesis: Option<String>,
}

struct Inner {
    chain_id: u64,
    endpoints: Vec<Endpoint>,
    client: Client,
    config: RpcConfig,
    state: Mutex<State>,
    notify: Notify,
    ids: AtomicU64,
}

/// Clones share limits, identity pins, health state, and HTTP connections.
#[derive(Clone)]
pub struct RpcPool {
    inner: Arc<Inner>,
}

impl fmt::Debug for RpcPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RpcPool")
            .field("chain_id", &self.inner.chain_id)
            .field("endpoint_count", &self.inner.endpoints.len())
            .field("health", &self.health())
            .finish()
    }
}

// Admission is reserved under the state lock and released even if the caller's
// future is cancelled. No waiting operation holds a synchronous lock.
struct Lease {
    inner: Arc<Inner>,
    index: usize,
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state.endpoints[self.index].in_flight -= 1;
        drop(state);
        self.inner.notify.notify_waiters();
    }
}

impl RpcPool {
    pub fn new(chain_id: u64, urls: Vec<String>, config: RpcConfig) -> Result<Self, RpcError> {
        if chain_id == 0
            || urls.is_empty()
            || urls.len() > 32
            || config.max_attempts == 0
            || config.max_attempts > 100
            || config.timeout_ms == 0
            || config.total_timeout_ms == 0
            || config.max_response_bytes == 0
            || config.max_response_bytes > 1024 * 1024 * 1024
            || config.max_in_flight_per_endpoint == 0
            || config.max_in_flight_per_endpoint > 1024
            || config.requests_per_second == Some(0)
            || config.base_backoff_ms > config.max_backoff_ms
            || [
                config.timeout_ms,
                config.total_timeout_ms,
                config.max_backoff_ms,
                config.cooldown_ms,
                config.identity_ttl_ms,
            ]
            .into_iter()
            .any(|n| n > 86_400_000)
        {
            return Err(RpcError::new(
                "invalid_config",
                "Invalid RPC pool limits or chain ID",
            ));
        }
        let mut seen = HashSet::new();
        let mut endpoints = Vec::new();
        for url in urls {
            let parsed = url::Url::parse(&url)
                .map_err(|_| RpcError::new("invalid_config", "Invalid RPC endpoint URL"))?;
            if !matches!(parsed.scheme(), "http" | "https")
                || parsed.host_str().is_none()
                || parsed.fragment().is_some()
                || !seen.insert(url.clone())
            {
                return Err(RpcError::new(
                    "invalid_config",
                    "RPC endpoints must be distinct HTTP(S) URLs without fragments",
                ));
            }
            endpoints.push(Endpoint {
                url,
                identity_lock: AsyncMutex::new(()),
                send_lock: AsyncMutex::new(()),
            });
        }
        let genesis = config
            .expected_genesis_hash
            .as_deref()
            .map(normalize_hash)
            .transpose()?;
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()
            .map_err(|_| RpcError::new("invalid_config", "Cannot construct RPC HTTP client"))?;
        Ok(Self {
            inner: Arc::new(Inner {
                chain_id,
                state: Mutex::new(State {
                    endpoints: (0..endpoints.len())
                        .map(|_| EndpointState::default())
                        .collect(),
                    cursor: 0,
                    genesis,
                }),
                endpoints,
                client,
                config,
                notify: Notify::new(),
                ids: AtomicU64::new(1),
            }),
        })
    }

    pub fn chain_id(&self) -> u64 {
        self.inner.chain_id
    }

    /// Returns the operator pin or the genesis established by a validated endpoint.
    pub fn genesis_hash(&self) -> Option<String> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .genesis
            .clone()
    }

    /// Health data uses stable local IDs rather than provider URLs.
    pub fn health(&self) -> Vec<EndpointHealth> {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        state
            .endpoints
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let mut unsupported_methods: Vec<_> = e.unsupported.iter().cloned().collect();
                unsupported_methods.sort();
                EndpointHealth {
                    endpoint: format!("rpc-{}", i + 1),
                    status: if e.quarantined {
                        "quarantined"
                    } else if e.cool_until.is_some_and(|t| t > now) {
                        "cooldown"
                    } else if !e.identity_until.is_some_and(|until| until > now) {
                        "unverified"
                    } else {
                        "ready"
                    }
                    .into(),
                    in_flight: e.in_flight,
                    requests: e.requests,
                    successes: e.successes,
                    failures: e.failures,
                    consecutive_failures: e.consecutive_failures,
                    latency_ms: e.latency_ms,
                    unsupported_methods,
                }
            })
            .collect()
    }

    /// Execute an allowlisted read method with a bounded attempt and time budget.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        if !is_read_method(method) {
            return Err(RpcError::new(
                "method_not_allowed",
                "RPC pool accepts allowlisted read methods only",
            ));
        }
        if !params.is_array() {
            return Err(RpcError::new(
                "invalid_request",
                "RPC parameters must be an array",
            ));
        }
        let deadline = Instant::now() + Duration::from_millis(self.inner.config.total_timeout_ms);
        timeout_at(
            TokioInstant::from_std(deadline),
            self.call_until(method, params, deadline),
        )
        .await
        .map_err(|_| RpcError::new("timeout", "RPC operation exceeded its total time budget"))?
    }

    async fn call_until(
        &self,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> Result<Value, RpcError> {
        let mut last_error = RpcError::new("unavailable", "No eligible RPC endpoint is available");
        let mut missing_history = HashSet::new();
        for attempt in 0..self.inner.config.max_attempts {
            let lease = match self.select(method, deadline, &missing_history).await {
                Ok(lease) => lease,
                Err(e) => return Err(e),
            };
            let index = lease.index;
            let started = Instant::now();
            let outcome = async {
                self.validate_identity(index, deadline).await?;
                self.request(index, method, params.clone(), deadline).await
            }
            .await;
            self.record(index, method, started.elapsed(), outcome.as_ref().err());
            drop(lease);
            match outcome {
                Ok(result) => return Ok(result),
                Err(e) => {
                    let retry = e.retryable()
                        || matches!(
                            e.category.as_str(),
                            "identity" | "unsupported" | "history_unavailable"
                        );
                    if !retry {
                        return Err(e);
                    }
                    if e.category == "history_unavailable" {
                        missing_history.insert(index);
                    }
                    let transient = e.retryable();
                    last_error = e;
                    if attempt + 1 == self.inner.config.max_attempts {
                        break;
                    }
                    // Endpoint-local cooldown protects rate-limited providers.
                    // Backoff bounds the caller's retry rate while allowing a healthy peer.
                    if transient {
                        let exponent = 1u64.checked_shl(attempt.min(30)).unwrap_or(u64::MAX);
                        let cap = self
                            .inner
                            .config
                            .base_backoff_ms
                            .saturating_mul(exponent)
                            .min(self.inner.config.max_backoff_ms);
                        let jitter = if cap == 0 {
                            0
                        } else {
                            rand::random_range(cap / 2..=cap)
                        };
                        self.sleep_before(Duration::from_millis(jitter), deadline)
                            .await?;
                    }
                }
            }
        }
        Err(last_error)
    }

    async fn select(
        &self,
        method: &str,
        deadline: Instant,
        missing_history: &HashSet<usize>,
    ) -> Result<Lease, RpcError> {
        loop {
            // Register before checking state to avoid a lost release notification.
            let notified = self.inner.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let mut wake = None;
            {
                let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                let now = Instant::now();
                let len = state.endpoints.len();
                let mut best: Option<(usize, usize)> = None;
                let mut non_quarantined = 0;
                let mut capable = 0;
                for offset in 0..len {
                    let i = (state.cursor + offset) % len;
                    let e = &state.endpoints[i];
                    if e.quarantined {
                        continue;
                    }
                    non_quarantined += 1;
                    if e.unsupported.contains(method) || missing_history.contains(&i) {
                        continue;
                    }
                    capable += 1;
                    if let Some(until) = e.cool_until.filter(|t| *t > now) {
                        wake = Some(wake.map_or(until, |old: Instant| old.min(until)));
                        continue;
                    }
                    if e.in_flight >= self.inner.config.max_in_flight_per_endpoint {
                        continue;
                    }
                    if best.is_none_or(|(_, load)| e.in_flight < load) {
                        best = Some((i, e.in_flight));
                    }
                }
                if non_quarantined == 0 {
                    return Err(RpcError::new(
                        "identity",
                        "All RPC endpoints are quarantined after identity validation failures",
                    ));
                }
                if capable == 0 && !missing_history.is_empty() {
                    return Err(RpcError::new(
                        "history_unavailable",
                        "Requested historical block is unavailable on eligible RPC endpoints",
                    ));
                }
                if capable == 0 {
                    return Err(RpcError::new(
                        "unsupported",
                        "No RPC endpoint supports the requested method",
                    ));
                }
                if let Some((index, _)) = best {
                    state.endpoints[index].in_flight += 1;
                    state.cursor = (index + 1) % len;
                    return Ok(Lease {
                        inner: self.inner.clone(),
                        index,
                    });
                }
            }
            let until = wake.unwrap_or(deadline).min(deadline);
            tokio::select! {
                _ = &mut notified => {},
                _ = tokio::time::sleep_until(TokioInstant::from_std(until)) => {},
            }
            if Instant::now() >= deadline {
                return Err(RpcError::new("timeout", "RPC endpoint admission timed out"));
            }
        }
    }

    async fn validate_identity(&self, index: usize, deadline: Instant) -> Result<(), RpcError> {
        let _guard = self.inner.endpoints[index].identity_lock.lock().await;
        {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.endpoints[index].quarantined {
                return Err(RpcError::new("identity", "RPC endpoint is quarantined"));
            }
            if state.endpoints[index]
                .identity_until
                .is_some_and(|until| until > Instant::now())
            {
                return Ok(());
            }
        }
        let chain = self
            .request(index, "eth_chainId", json!([]), deadline)
            .await?;
        let got = chain
            .as_str()
            .ok_or_else(|| RpcError::new("invalid_response", "Invalid RPC chain identity"))?;
        if quantity(got)? != self.inner.chain_id {
            return Err(RpcError::new(
                "identity",
                "RPC chain ID does not match configured chain",
            ));
        }
        let genesis = self
            .request(
                index,
                "eth_getBlockByNumber",
                json!(["0x0", false]),
                deadline,
            )
            .await?;
        if genesis.is_null() {
            return Err(RpcError::new(
                "history_unavailable",
                "RPC genesis block is unavailable",
            ));
        }
        let genesis = genesis
            .get("hash")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::new("invalid_response", "RPC genesis block is unavailable"))?;
        let hash = normalize_hash(genesis)?;
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if state
            .genesis
            .as_ref()
            .is_some_and(|expected| expected != &hash)
        {
            return Err(RpcError::new(
                "identity",
                "RPC genesis does not match configured chain",
            ));
        }
        state.genesis = Some(hash);
        state.endpoints[index].identity_until =
            Some(Instant::now() + Duration::from_millis(self.inner.config.identity_ttl_ms));
        Ok(())
    }

    async fn request(
        &self,
        index: usize,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> Result<Value, RpcError> {
        let request_deadline =
            deadline.min(Instant::now() + Duration::from_millis(self.inner.config.timeout_ms));
        timeout_at(
            TokioInstant::from_std(request_deadline),
            self.request_inner(index, method, params),
        )
        .await
        .map_err(|_| RpcError::new("timeout", "RPC request timed out"))?
    }

    async fn request_inner(
        &self,
        index: usize,
        method: &str,
        params: Value,
    ) -> Result<Value, RpcError> {
        // Serialize dispatch admission, not response reads. Recheck cooldown after
        // every wait so requests already admitted at the operation level cannot
        // bypass a later Retry-After. Cancelled waiters consume no future slots.
        let gate = self.inner.endpoints[index].send_lock.lock().await;
        loop {
            let wait_until = {
                let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                let e = &mut state.endpoints[index];
                let now = Instant::now();
                let ready = e.cool_until.unwrap_or(now).max(
                    if self.inner.config.requests_per_second.is_some() {
                        e.next_request.unwrap_or(now)
                    } else {
                        now
                    },
                );
                if ready > now {
                    Some(ready)
                } else {
                    if let Some(rate) = self.inner.config.requests_per_second {
                        e.next_request = Some(now + Duration::from_secs_f64(1.0 / f64::from(rate)));
                    }
                    e.requests += 1;
                    None
                }
            };
            let Some(until) = wait_until else {
                break;
            };
            tokio::time::sleep_until(TokioInstant::from_std(until)).await;
        }
        drop(gate);
        let historical_block = is_historical_block_request(method, &params);
        let id = self.inner.ids.fetch_add(1, Ordering::Relaxed);
        let mut response = self
            .inner
            .client
            .post(&self.inner.endpoints[index].url)
            .json(&json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))
            .send()
            .await
            .map_err(transport_error)?;
        let status = response.status();
        if !status.is_success() {
            let mut error = if status == StatusCode::REQUEST_TIMEOUT
                || status == StatusCode::TOO_MANY_REQUESTS
                || status.is_server_error()
            {
                RpcError::new("transient", "RPC provider returned a retryable HTTP status")
            } else {
                RpcError::new("http", "RPC provider returned a non-retryable HTTP status")
            };
            if matches!(
                status,
                StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
            ) {
                error.retry_after = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|h| h.to_str().ok())
                    .and_then(parse_retry_after);
            }
            return Err(error);
        }
        let limit = self.inner.config.max_response_bytes;
        if response.content_length().is_some_and(|n| n > limit as u64) {
            return Err(RpcError::new(
                "too_large",
                "RPC response exceeds the configured size limit",
            ));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
            if chunk.len() > limit.saturating_sub(body.len()) {
                return Err(RpcError::new(
                    "too_large",
                    "RPC response exceeds the configured size limit",
                ));
            }
            body.extend_from_slice(&chunk);
        }
        let payload: Value = serde_json::from_slice(&body)
            .map_err(|_| RpcError::new("invalid_response", "RPC response is not valid JSON"))?;
        if payload.get("jsonrpc") != Some(&json!("2.0"))
            || payload.get("id") != Some(&json!(id))
            || payload.get("result").is_some() == payload.get("error").is_some()
        {
            return Err(RpcError::new(
                "invalid_response",
                "RPC response envelope does not match the request",
            ));
        }
        if let Some(error) = payload.get("error") {
            let code = error.get("code").and_then(Value::as_i64).ok_or_else(|| {
                RpcError::new("invalid_response", "RPC error envelope is invalid")
            })?;
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .ok_or_else(|| RpcError::new("invalid_response", "RPC error envelope is invalid"))?
                .to_ascii_lowercase();
            let category = if code == -32601 {
                "unsupported"
            } else if ((-32099..=-32000).contains(&code) || code == -32603)
                && (message.contains("rate limit")
                    || message.contains("too many requests")
                    || message.contains("timeout")
                    || message.contains("temporarily unavailable"))
            {
                "transient"
            } else if code == -32005
                || [
                    "too many results",
                    "response size",
                    "block range",
                    "limit exceeded",
                ]
                .iter()
                .any(|s| message.contains(s))
            {
                "too_large"
            } else {
                "rpc"
            };
            return Err(RpcError::new(
                category,
                match category {
                    "unsupported" => "RPC method is unsupported by this endpoint",
                    "transient" => "RPC provider reported a transient failure",
                    "too_large" => "RPC provider rejected the response size or block range",
                    _ => "RPC provider rejected the request",
                },
            ));
        }
        if payload["result"].is_null() && historical_block {
            return Err(RpcError::new(
                "history_unavailable",
                "RPC endpoint cannot serve the requested historical block",
            ));
        }
        Ok(payload["result"].clone())
    }

    fn record(&self, index: usize, method: &str, latency: Duration, error: Option<&RpcError>) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let e = &mut state.endpoints[index];
        let elapsed = latency.as_millis().min(u64::MAX as u128) as u64;
        e.latency_ms = Some(e.latency_ms.map_or(elapsed, |old| {
            old.saturating_mul(3).saturating_add(elapsed) / 4
        }));
        if let Some(error) = error {
            e.failures += 1;
            e.consecutive_failures = e.consecutive_failures.saturating_add(1);
            if error.category == "identity" {
                e.quarantined = true;
                e.identity_until = None;
            }
            if error.category == "unsupported" {
                e.unsupported.insert(method.to_string());
            }
            if error.retryable() {
                let exponent = 1u64
                    .checked_shl(e.consecutive_failures.saturating_sub(1).min(16))
                    .unwrap_or(u64::MAX);
                let cooldown = Duration::from_millis(
                    self.inner
                        .config
                        .cooldown_ms
                        .saturating_mul(exponent)
                        .min(86_400_000),
                );
                let delay = cooldown.max(error.retry_after.unwrap_or_default());
                // Retry-After can be longer than this operation's budget. Retain
                // the embargo across callers rather than shortening it to retry.
                let until = Instant::now() + delay;
                e.cool_until = Some(e.cool_until.map_or(until, |existing| existing.max(until)));
                e.identity_until = None;
            }
        } else {
            e.successes += 1;
            e.consecutive_failures = 0;
            if e.cool_until.is_some_and(|until| until <= Instant::now()) {
                e.cool_until = None;
            }
        }
    }

    async fn sleep_before(&self, duration: Duration, deadline: Instant) -> Result<(), RpcError> {
        let until = Instant::now() + duration;
        if until >= deadline {
            return Err(RpcError::new("timeout", "RPC retry budget exhausted"));
        }
        tokio::time::sleep_until(TokioInstant::from_std(until)).await;
        Ok(())
    }
}

fn is_historical_block_request(method: &str, params: &Value) -> bool {
    let Some(reference) = params.get(0).and_then(Value::as_str) else {
        return false;
    };
    match method {
        "eth_getBlockByNumber" => quantity(reference).is_ok(),
        "eth_getBlockByHash" => normalize_hash(reference).is_ok(),
        _ => false,
    }
}

fn transport_error(error: reqwest::Error) -> RpcError {
    if error.is_timeout() {
        RpcError::new("timeout", "RPC request timed out")
    } else {
        RpcError::new("transient", "RPC transport failed")
    }
}

fn parse_retry_after(value: &str) -> Option<Duration> {
    let duration = value
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
        .or_else(|| {
            httpdate::parse_http_date(value)
                .ok()
                .map(|t| t.duration_since(SystemTime::now()).unwrap_or_default())
        })?;
    // Cap untrusted values to one day, sufficient to prevent provider retry storms
    // without risking Instant overflow or an effectively permanent rate embargo.
    Some(duration.min(Duration::from_secs(86_400)))
}

pub fn is_read_method(method: &str) -> bool {
    matches!(
        method,
        "eth_chainId"
            | "eth_blockNumber"
            | "eth_getBlockByNumber"
            | "eth_getBlockByHash"
            | "eth_getTransactionByHash"
            | "eth_getTransactionByBlockHashAndIndex"
            | "eth_getTransactionByBlockNumberAndIndex"
            | "eth_getTransactionReceipt"
            | "eth_getBlockReceipts"
            | "eth_getLogs"
            | "eth_getCode"
            | "eth_getStorageAt"
            | "eth_getBalance"
            | "eth_getTransactionCount"
            | "eth_call"
            | "eth_estimateGas"
            | "eth_getProof"
            | "eth_gasPrice"
            | "eth_feeHistory"
            | "eth_maxPriorityFeePerGas"
            | "debug_traceBlockByNumber"
            | "debug_traceBlockByHash"
            | "debug_traceTransaction"
            | "trace_block"
            | "trace_transaction"
            | "trace_replayBlockTransactions"
            | "net_version"
            | "web3_clientVersion"
    )
}

pub fn normalize_address(value: &str) -> Result<String, RpcError> {
    normalize_hex(value, 20)
}
pub fn normalize_hash(value: &str) -> Result<String, RpcError> {
    normalize_hex(value, 32)
}
fn normalize_hex(value: &str, bytes: usize) -> Result<String, RpcError> {
    if value.len() != bytes * 2 + 2
        || !value.starts_with("0x")
        || !value[2..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(RpcError::new(
            "invalid_response",
            "Invalid EVM address or hash",
        ));
    }
    Ok(value.to_ascii_lowercase())
}

pub fn quantity(value: &str) -> Result<u64, RpcError> {
    let hex = value
        .strip_prefix("0x")
        .filter(|s| !s.is_empty() && s.len() <= 16)
        .filter(|s| s.len() == 1 || !s.starts_with('0'))
        .filter(|s| s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| RpcError::new("invalid_response", "Invalid EVM quantity"))?;
    u64::from_str_radix(hex, 16)
        .map_err(|_| RpcError::new("invalid_response", "EVM quantity exceeds u64"))
}
