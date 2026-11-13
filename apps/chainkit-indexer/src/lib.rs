//! Configuration, HTTP, and stdio MCP adapters for the reusable indexing engine.

pub use chainkit_api::{envelope, http, mcp, openapi};

use anyhow::{bail, Context, Result};
use chainkit_abi::{AbiResolver, AbiResolverConfig};
use chainkit_evm::{RpcConfig, RpcPool};
use chainkit_indexer::{ScanOptions, Store};
use serde::Deserialize;
use std::{collections::BTreeMap, path::Path};

/// Operator-only configuration. Never serialize or log credentials.
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub database: String,
    pub listen: String,
    pub poll_interval_ms: u64,
    pub rpc: RpcConfig,
    pub scanner: ScanOptions,
    pub abi: AbiResolverConfig,
    #[serde(deserialize_with = "deserialize_chains")]
    pub chains: BTreeMap<u64, Vec<String>>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            database: "chainkit.sqlite".into(),
            listen: "127.0.0.1:8080".into(),
            poll_interval_ms: 5_000,
            rpc: RpcConfig::default(),
            scanner: ScanOptions::default(),
            abi: AbiResolverConfig::default(),
            chains: BTreeMap::new(),
        }
    }
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let mut config = match path {
            Some(path) => {
                let text =
                    std::fs::read_to_string(path).context("Could not read configuration file")?;
                toml::from_str::<Self>(&text)
                    .map_err(|_| anyhow::anyhow!("Invalid TOML configuration"))?
            }
            None => Self::default(),
        };
        // Provider keys are optional environment input and are never returned by APIs.
        if let Ok(key) = std::env::var("CHAINKIT_ETHERSCAN_API_KEY") {
            config.abi.etherscan_api_key = Some(key);
        }
        if config.poll_interval_ms == 0 {
            bail!("poll_interval_ms must be positive");
        }
        Ok(config)
    }

    pub fn pools(&self) -> Result<BTreeMap<u64, RpcPool>> {
        self.chains
            .iter()
            .map(|(&id, urls)| {
                RpcPool::new(id, urls.clone(), self.rpc.clone())
                    .map(|pool| (id, pool))
                    .map_err(Into::into)
            })
            .collect()
    }
}

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub config: Config,
    pub pools: BTreeMap<u64, RpcPool>,
    pub resolver: AbiResolver,
}

impl AppState {
    pub fn new(config: Config) -> Result<Self> {
        let pools = config.pools()?;
        let store = Store::open(&config.database)?;
        let resolver = AbiResolver::new(config.abi.clone())?;
        Ok(Self {
            store,
            config,
            pools,
            resolver,
        })
    }
}

impl AppState {
    pub fn api(&self) -> chainkit_api::ApiState {
        chainkit_api::ApiState {
            store: self.store.clone(),
            resolver: self.resolver.clone(),
            chain_ids: self.pools.keys().copied().collect(),
        }
    }
}

fn deserialize_chains<'de, D>(
    deserializer: D,
) -> std::result::Result<BTreeMap<u64, Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let values = BTreeMap::<String, Vec<String>>::deserialize(deserializer)?;
    let mut result = BTreeMap::new();
    for (key, endpoints) in values {
        let id = key
            .parse::<u64>()
            .ok()
            .filter(|id| *id > 0)
            .ok_or_else(|| serde::de::Error::custom("Chain keys must be positive integer IDs"))?;
        if result.insert(id, endpoints).is_some() {
            return Err(serde::de::Error::custom("Duplicate numeric chain ID"));
        }
    }
    Ok(result)
}
