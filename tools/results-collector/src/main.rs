use civpulse_results_collector::{
    Error, Result,
    config::{Config, RPC_DEADLINE, RPC_RESPONSE_LIMIT},
    job::UnsupportedProcessor,
    rpc::Rpc,
    validate,
    worker::{self, Options},
};
use std::{process::ExitCode, sync::Arc};
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    // Never render dependency error chains: they can contain credentials or source data.
    match execute().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
async fn execute() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|argument| argument == "validate") {
        return validate::run(&args[1..], shutdown_signal()).await;
    }
    let command = match args.as_slice() {
        [] => "daemon",
        [command] => command.as_str(),
        _ => {
            return Err(Error::Config(
                "Usage: civpulse-results-collector [daemon|once|health|liveness|validate|--check|--help]",
            ));
        }
    };
    if matches!(command, "--help" | "-h") {
        println!(
            "civpulse-results-collector [daemon|once|health|liveness|validate|--check]\nCollection defaults to disabled. health checks service RPC access; liveness checks the local daemon heartbeat. --check performs no I/O. validate <path> <election-date> [expected-jurisdictions expected-items] validates local bytes without credentials."
        );
        return Ok(());
    }
    if command == "--check" {
        return Ok(());
    }
    if !matches!(command, "daemon" | "once" | "health" | "liveness") {
        return Err(Error::Config("Unknown collector command"));
    }
    let config = Config::read(|key| std::env::var(key).ok())?;
    if command == "liveness" {
        return worker::liveness(&config.heartbeat_path).await;
    }
    let filter = EnvFilter::try_from_default_env().or_else(|_| {
        if std::env::var_os("RUST_LOG").is_none() {
            Ok(EnvFilter::new("info"))
        } else {
            Err(Error::Config("Invalid logging filter"))
        }
    })?;
    tracing_subscriber::fmt()
        .json()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .try_init()
        .map_err(|_| Error::Config("Cannot initialize logging"))?;
    let rpc = if config.enabled || command == "health" {
        Some(Rpc::new(
            config
                .rpc_url
                .as_deref()
                .ok_or(Error::Config("SUPABASE_URL is required"))?,
            config
                .rpc_key
                .as_deref()
                .ok_or(Error::Config("SUPABASE_SERVICE_ROLE_KEY is required"))?,
            RPC_DEADLINE,
            RPC_RESPONSE_LIMIT,
        )?)
    } else {
        None
    };
    if command == "health" {
        let health: serde_json::Value = rpc
            .as_ref()
            .ok_or(Error::Config("Missing health RPC"))?
            .call("get_sos_collection_health", serde_json::json!({}))
            .await?;
        if !health.is_array() {
            return Err(Error::InvalidResponse);
        }
        println!(
            "{}",
            serde_json::to_string(&health).map_err(|_| Error::InvalidResponse)?
        );
        return Ok(());
    }
    let options = Options {
        enabled: config.enabled,
        concurrency: config.concurrency,
        heartbeat_path: Some(config.heartbeat_path),
        ..Options::default()
    };
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let signal = tokio::spawn(async move {
        if shutdown_signal().await.is_ok() {
            let _ = shutdown_tx.send(true);
        }
    });
    let result = if command == "once" {
        let rpc = rpc.as_ref().ok_or(Error::Disabled)?;
        let once = worker::once(rpc, Arc::new(UnsupportedProcessor), &options);
        tokio::pin!(once);
        tokio::select! {
            result = &mut once => result.map(|_| ()),
            _ = shutdown_rx.changed() => tokio::time::timeout(options.drain_timeout, &mut once).await.map_err(|_| Error::Shutdown)?.map(|_| ()),
        }
    } else {
        worker::daemon(rpc, Arc::new(UnsupportedProcessor), options, shutdown_rx).await
    };
    signal.abort();
    result
}
#[cfg(unix)]
async fn shutdown_signal() -> std::io::Result<()> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! { result = tokio::signal::ctrl_c() => result, _ = terminate.recv() => Ok(()) }
}
#[cfg(not(unix))]
async fn shutdown_signal() -> std::io::Result<()> {
    tokio::signal::ctrl_c().await
}
