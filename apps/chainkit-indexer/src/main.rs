use anyhow::{bail, Context, Result};
use chainkit_indexer::{scan_once, Query};
use chainkit_indexer_cli::{envelope, http, mcp, openapi, AppState, Config};
use clap::{Args, Parser, Subcommand};
use serde_json::{json, Value};
use std::{path::PathBuf, time::Duration};
use tokio::io::BufReader;
use tracing::{info, warn};

#[derive(Parser)]
#[command(
    name = "chainkit-indexer",
    version,
    about = "Multi-project EVM contract evidence indexer"
)]
struct Cli {
    #[arg(long, global = true, env = "CHAINKIT_CONFIG")]
    config: Option<PathBuf>,
    #[arg(long, global = true, env = "CHAINKIT_DATABASE")]
    database: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a project locally; save its management token securely.
    ProjectCreate {
        project: String,
        #[arg(long)]
        name: Option<String>,
    },
    /// List projects. This administrative operation is not exposed over HTTP.
    ProjectList,
    /// Rotate the management token and revoke its previous value.
    ProjectRotateKey { project: String },
    /// Register a contract. ABI is discovered automatically unless supplied.
    ContractAdd {
        #[command(flatten)]
        identity: Identity,
        #[arg(long)]
        start_block: u64,
        #[arg(long,default_value="full",value_parser=["basic","full"])]
        mode: String,
        #[command(flatten)]
        abi: AbiInput,
    },
    Contracts {
        #[arg(long)]
        project: String,
    },
    /// Run one bounded scan round across configured chains.
    Scan {
        #[arg(long)]
        chain_id: Option<u64>,
    },
    Coverage {
        #[command(flatten)]
        identity: Identity,
    },
    Transactions {
        #[command(flatten)]
        identity: Identity,
        #[command(flatten)]
        query: QueryArgs,
    },
    Events {
        #[command(flatten)]
        identity: Identity,
        #[command(flatten)]
        query: QueryArgs,
    },
    Context {
        #[command(flatten)]
        identity: Identity,
        #[command(flatten)]
        query: QueryArgs,
    },
    Abis {
        #[command(flatten)]
        identity: Identity,
    },
    Pause {
        #[command(flatten)]
        identity: Identity,
    },
    Resume {
        #[command(flatten)]
        identity: Identity,
    },
    Backfill {
        #[command(flatten)]
        identity: Identity,
        #[arg(long)]
        from_block: u64,
    },
    /// Bind a new ABI at a historical block and update indexed decodings.
    AbiSet {
        #[command(flatten)]
        identity: Identity,
        #[arg(long)]
        effective_from: u64,
        #[command(flatten)]
        abi: AbiInput,
    },
    /// Probe a read request and show sanitized pool health.
    RpcHealth {
        #[arg(long)]
        chain_id: Option<u64>,
    },
    /// Run HTTP queries and background indexing in one process.
    Serve {
        #[arg(long)]
        listen: Option<String>,
    },
    /// Export the machine-readable HTTP API contract.
    Openapi,
    /// Serve read-only MCP tools over stdin/stdout. Run serve separately for indexing.
    Mcp,
}

#[derive(Args)]
struct Identity {
    #[arg(long)]
    project: String,
    #[arg(long)]
    chain_id: u64,
    #[arg(long)]
    address: String,
}

#[derive(Args)]
struct AbiInput {
    #[arg(long, conflicts_with = "abi_json")]
    abi_file: Option<PathBuf>,
    #[arg(long, conflicts_with = "abi_file")]
    abi_json: Option<String>,
}

#[derive(Args, Default)]
struct QueryArgs {
    #[arg(long)]
    limit: Option<usize>,
    #[arg(long)]
    cursor: Option<String>,
    #[arg(long)]
    from_block: Option<u64>,
    #[arg(long)]
    to_block: Option<u64>,
    #[arg(long)]
    tx_hash: Option<String>,
    #[arg(long)]
    sender: Option<String>,
    #[arg(long)]
    status: Option<u64>,
    #[arg(long)]
    signature: Option<String>,
    #[arg(long)]
    topic0: Option<String>,
}
impl From<QueryArgs> for Query {
    fn from(q: QueryArgs) -> Self {
        Self {
            limit: q.limit,
            cursor: q.cursor,
            from_block: q.from_block,
            to_block: q.to_block,
            tx_hash: q.tx_hash,
            sender: q.sender,
            status: q.status,
            signature: q.signature,
            topic0: q.topic0,
        }
    }
}

async fn resolve_abi(state: &AppState, id: &Identity, input: AbiInput) -> Result<(Value, String)> {
    let manual = if let Some(path) = input.abi_file {
        if std::fs::metadata(&path)
            .context("Could not inspect ABI file")?
            .len()
            > 2 * 1024 * 1024
        {
            bail!("ABI file exceeds 2 MiB limit");
        }
        Some(std::fs::read_to_string(path).context("Could not read ABI file")?)
    } else {
        input.abi_json
    };
    if let Some(text) = manual {
        return Ok((
            serde_json::from_str(&text).context("ABI must be valid JSON")?,
            "manual".into(),
        ));
    }
    let resolved=state.resolver.resolve(id.chain_id,&id.address).await
        .context("Automatic ABI lookup failed; supply --abi-file or --abi-json, or [] to collect raw evidence")?;
    Ok((resolved.abi, resolved.source))
}

fn print_json(value: Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&envelope(value))?);
    Ok(())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    if let Err(error) = run(Cli::parse()).await {
        eprintln!(
            "{}",
            json!({"schema_version":"1","error":{"code":"command_failed","message":error.to_string()}})
        );
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    if matches!(&cli.command, Command::Openapi) {
        println!("{}", serde_json::to_string_pretty(&openapi::document())?);
        return Ok(());
    }
    let mut config = Config::load(cli.config.as_deref())?;
    if let Some(database) = cli.database {
        config.database = database;
    }
    let state = AppState::new(config)?;
    match cli.command {
        Command::ProjectCreate { project, name } => print_json(
            state
                .store
                .create_project(&project, name.as_deref().unwrap_or(&project))?,
        ),
        Command::ProjectList => print_json(state.store.projects()?),
        Command::ProjectRotateKey { project } => print_json(state.store.rotate_key(&project)?),
        Command::Contracts { project } => print_json(state.store.contracts(&project)?),
        Command::ContractAdd {
            identity: id,
            start_block,
            mode,
            abi,
        } => {
            if !state.pools.contains_key(&id.chain_id) {
                bail!("Chain must be configured before registering a contract");
            }
            let (abi, source) = resolve_abi(&state, &id, abi).await?;
            let result = state.store.register_with_source(
                &id.project,
                id.chain_id,
                &id.address,
                abi,
                start_block,
                &mode,
                &source,
            )?;
            print_json(json!({"registration":result,"abi_source":source}))
        }
        Command::Scan { chain_id } => {
            let mut results = Vec::new();
            let pools: Vec<_> = state
                .pools
                .values()
                .filter(|p| chain_id.is_none_or(|id| id == p.chain_id()))
                .collect();
            if pools.is_empty() {
                bail!("No matching configured chain");
            }
            for pool in pools {
                results.push(scan_once(&state.store, pool, &state.config.scanner).await?);
            }
            let failed = results.iter().any(|v| {
                v.get("errors")
                    .and_then(Value::as_array)
                    .is_some_and(|e| !e.is_empty())
            });
            print_json(json!(results))?;
            if failed {
                bail!("Scan round completed with registration errors; inspect the JSON report");
            }
            Ok(())
        }
        Command::Coverage { identity: i } => {
            print_json(state.store.coverage(&i.project, i.chain_id, &i.address)?)
        }
        Command::Transactions {
            identity: i,
            query: q,
        } => print_json(state.store.query(
            &i.project,
            i.chain_id,
            &i.address,
            "transactions",
            &q.into(),
        )?),
        Command::Events {
            identity: i,
            query: q,
        } => print_json(state.store.query(
            &i.project,
            i.chain_id,
            &i.address,
            "events",
            &q.into(),
        )?),
        Command::Context {
            identity: i,
            query: q,
        } => {
            let tx_hash = q.tx_hash.clone().context("context requires --tx-hash")?;
            print_json(state.store.context(
                &i.project,
                i.chain_id,
                &i.address,
                &tx_hash,
                &q.into(),
            )?)
        }
        Command::Abis { identity: i } => print_json(
            state
                .store
                .abi_history(&i.project, i.chain_id, &i.address)?,
        ),
        Command::Pause { identity: i } => print_json(
            state
                .store
                .set_paused(&i.project, i.chain_id, &i.address, true)?,
        ),
        Command::Resume { identity: i } => print_json(
            state
                .store
                .set_paused(&i.project, i.chain_id, &i.address, false)?,
        ),
        Command::Backfill {
            identity: i,
            from_block,
        } => print_json(
            state
                .store
                .backfill(&i.project, i.chain_id, &i.address, from_block)?,
        ),
        Command::AbiSet {
            identity: i,
            effective_from,
            abi,
        } => {
            let (abi, source) = resolve_abi(&state, &i, abi).await?;
            let result = state.store.update_abi_with_source(
                &i.project,
                i.chain_id,
                &i.address,
                abi,
                effective_from,
                &source,
            )?;
            print_json(json!({"binding":result,"abi_source":source}))
        }
        Command::RpcHealth { chain_id } => {
            let mut result = Vec::new();
            for pool in state
                .pools
                .values()
                .filter(|p| chain_id.is_none_or(|id| id == p.chain_id()))
            {
                let probe = match pool.call("eth_blockNumber", json!([])).await {
                    Ok(head) => json!({"head":head}),
                    Err(error) => json!({"error":error.category}),
                };
                result.push(
                    json!({"chain_id":pool.chain_id(),"probe":probe,"endpoints":pool.health()}),
                );
            }
            if result.is_empty() {
                bail!("No matching configured chain");
            }
            print_json(json!(result))
        }
        Command::Serve { listen } => serve(state, listen).await,
        Command::Mcp => {
            mcp::run(
                &state.api(),
                BufReader::new(tokio::io::stdin()),
                tokio::io::stdout(),
            )
            .await
        }
        Command::Openapi => unreachable!(),
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("Install SIGTERM handler");
        tokio::select! { _=tokio::signal::ctrl_c()=>{}, _=terminate.recv()=>{} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn serve(state: AppState, listen: Option<String>) -> Result<()> {
    let listen = listen.unwrap_or_else(|| state.config.listen.clone());
    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .context("Could not bind HTTP listener")?;
    info!(address=%listener.local_addr()?,"HTTP service started");
    let scheduler_state = state.clone();
    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
    let mut scheduler = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(
            scheduler_state.config.poll_interval_ms,
        ));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _=stop_rx.changed()=>break,
                _=interval.tick()=>{
                    let mut tasks=tokio::task::JoinSet::new();
                    for pool in scheduler_state.pools.values() {
                        let pool=pool.clone();
                        let store=scheduler_state.store.clone();
                        let options=scheduler_state.config.scanner.clone();
                        tasks.spawn(async move {
                            if let Err(error)=scan_once(&store,&pool,&options).await {
                                warn!(chain_id=pool.chain_id(),category=%error.category,"Scan round failed; progress retained");
                            }
                        });
                    }
                    loop {
                        tokio::select! {
                            _=stop_rx.changed()=>{tasks.abort_all();return;},
                            result=tasks.join_next()=>if result.is_none(){break;},
                        }
                    }
                }
            }
        }
    });
    let shutdown_tx = stop_tx.clone();
    let result = axum::serve(listener, http::router(state.api()))
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            let _ = shutdown_tx.send(true);
        })
        .await;
    let _ = stop_tx.send(true);
    if tokio::time::timeout(Duration::from_secs(5), &mut scheduler)
        .await
        .is_err()
    {
        scheduler.abort();
    }
    result.context("HTTP service failed")
}
