use crate::{Abi, AbiError, MAX_ABI_BYTES};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AbiResolverConfig {
    pub sourcify_url: String,
    pub etherscan_url: Option<String>,
    pub etherscan_api_key: Option<String>,
    pub timeout_ms: u64,
}
impl Default for AbiResolverConfig {
    fn default() -> Self {
        Self {
            sourcify_url: "https://sourcify.dev/server".into(),
            etherscan_url: Some("https://api.etherscan.io/v2/api".into()),
            etherscan_api_key: None,
            timeout_ms: 15_000,
        }
    }
}
impl std::fmt::Debug for AbiResolverConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AbiResolverConfig")
            .field("etherscan_enabled", &self.etherscan_api_key.is_some())
            .field("timeout_ms", &self.timeout_ms)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolvedAbi {
    pub abi: Value,
    /// Provider identifier, never a credential-bearing URL. This is provenance,
    /// not proof of a match to every historical deployment or implementation.
    pub source: String,
}
#[derive(Clone)]
pub struct AbiResolver {
    client: reqwest::Client,
    config: AbiResolverConfig,
    cache: Arc<Mutex<AbiCache>>,
}
type AbiCache = HashMap<(u64, String), (Instant, ResolvedAbi)>;
impl std::fmt::Debug for AbiResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AbiResolver")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}
impl AbiResolver {
    pub fn new(config: AbiResolverConfig) -> Result<Self, AbiError> {
        if !(1..=120_000).contains(&config.timeout_ms) {
            return Err(AbiError::new(
                "configuration",
                "ABI discovery timeout must be between 1 and 120000 milliseconds",
            ));
        }
        for url in std::iter::once(&config.sourcify_url).chain(config.etherscan_url.iter()) {
            let parsed = url::Url::parse(url)
                .map_err(|_| AbiError::new("configuration", "Invalid ABI provider URL"))?;
            if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                return Err(AbiError::new(
                    "configuration",
                    "ABI providers require HTTP(S) URLs",
                ));
            }
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| {
                AbiError::new("configuration", "Unable to initialize ABI discovery client")
            })?;
        Ok(Self {
            client,
            config,
            cache: Arc::new(Mutex::new(HashMap::new())),
        })
    }
    pub async fn resolve(&self, chain_id: u64, address: &str) -> Result<ResolvedAbi, AbiError> {
        if chain_id == 0 {
            return Err(AbiError::new(
                "invalid_input",
                "A positive chain ID is required",
            ));
        }
        let address: alloy_primitives::Address = address
            .parse()
            .map_err(|_| AbiError::new("invalid_input", "Invalid contract address"))?;
        let address = format!("{address:#x}");
        let key = (chain_id, address.clone());
        if let Some((at, resolved)) = self
            .cache
            .lock()
            .map_err(|_| AbiError::new("internal", "ABI cache is unavailable"))?
            .get(&key)
        {
            if at.elapsed() < Duration::from_secs(300) {
                return Ok(resolved.clone());
            }
        }
        let mut sourcify =
            url::Url::parse(&self.config.sourcify_url).expect("URL validated on construction");
        let path = format!(
            "{}/v2/contract/{chain_id}/{address}",
            sourcify.path().trim_end_matches('/')
        );
        sourcify.set_path(&path);
        sourcify.query_pairs_mut().append_pair("fields", "abi");
        let resolved = match self.fetch(sourcify).await {
            Ok(payload) => payload
                .get("abi")
                .or_else(|| payload.pointer("/compilation/abi"))
                .cloned()
                .and_then(|abi| Abi::new(abi).ok())
                .map(|abi| ResolvedAbi {
                    abi: abi.value().clone(),
                    source: "sourcify".into(),
                }),
            Err(_) => None,
        };
        let resolved = if let Some(resolved) = resolved {
            Some(resolved)
        } else if let (Some(base), Some(api_key)) =
            (&self.config.etherscan_url, &self.config.etherscan_api_key)
        {
            let mut url = url::Url::parse(base).expect("URL validated on construction");
            url.query_pairs_mut().extend_pairs([
                ("chainid", chain_id.to_string().as_str()),
                ("module", "contract"),
                ("action", "getabi"),
                ("address", address.as_str()),
                ("apikey", api_key.as_str()),
            ]);
            match self.fetch(url).await {
                Ok(payload) if payload["status"].as_str() == Some("1") => payload["result"]
                    .as_str()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .and_then(|abi| Abi::new(abi).ok())
                    .map(|abi| ResolvedAbi {
                        abi: abi.value().clone(),
                        source: "etherscan".into(),
                    }),
                _ => None,
            }
        } else {
            None
        };
        let resolved = resolved.ok_or_else(|| AbiError::new("abi_unavailable", "No valid ABI is available from configured providers; supply an ABI manually or register an empty ABI to retain raw evidence"))?;
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| AbiError::new("internal", "ABI cache is unavailable"))?;
        if cache.len() >= 4096 {
            cache.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(300));
        }
        if cache.len() >= 4096 {
            cache.clear();
        }
        cache.insert(key, (Instant::now(), resolved.clone()));
        Ok(resolved)
    }
    async fn fetch(&self, url: url::Url) -> Result<Value, AbiError> {
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| AbiError::new("provider", "ABI provider request failed"))?;
        if !response.status().is_success() {
            return Err(AbiError::new(
                "provider",
                "ABI provider returned a non-success status",
            ));
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_ABI_BYTES as u64)
        {
            return Err(AbiError::new(
                "too_large",
                "ABI provider response exceeds limit",
            ));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| AbiError::new("provider", "ABI provider response failed"))?
        {
            if body.len().saturating_add(chunk.len()) > MAX_ABI_BYTES {
                return Err(AbiError::new(
                    "too_large",
                    "ABI provider response exceeds limit",
                ));
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body)
            .map_err(|_| AbiError::new("provider", "ABI provider returned invalid JSON"))
    }
}
