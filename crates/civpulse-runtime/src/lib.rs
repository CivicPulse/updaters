//! Shared process lifecycle for independently deployed `CivPulse` batch tools.

use std::future::Future;

use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;

/// Initialize JSON logs and run a batch operation until completion or cancellation.
///
/// Operations must be cancellation safe: SIGTERM drops the operation future. Persist
/// checkpoints externally and reconcile ambiguous remote writes before retrying.
///
/// # Errors
/// Returns an error for invalid logging configuration, operation failures, or signals.
pub async fn run<F>(tool: &'static str, operation: F) -> Result<()>
where
    F: Future<Output = Result<()>>,
{
    let filter = EnvFilter::try_from_default_env().or_else(|error| {
        if std::env::var_os("RUST_LOG").is_none() {
            Ok(EnvFilter::new("info"))
        } else {
            Err(error)
        }
    })?;
    tracing_subscriber::fmt()
        .json()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .try_init()
        .map_err(|error| anyhow::anyhow!("initialize logging: {error}"))?;

    tracing::info!(tool, "starting batch job");
    let result = tokio::select! {
        biased;
        signal = shutdown() => {
            signal.context("listen for shutdown")?;
            anyhow::bail!("{tool} interrupted; job did not complete")
        }
        result = operation => result,
    };
    match &result {
        Ok(()) => tracing::info!(tool, "batch job completed"),
        Err(error) => tracing::error!(tool, %error, "batch job failed"),
    }
    result
}

/// Run the starter command. Only an explicit scaffold check succeeds.
///
/// # Errors
/// Returns an error for an unimplemented operation or unsupported arguments.
pub async fn scaffold(tool: &'static str) -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [arg] if arg == "--help" || arg == "-h" => {
            println!(
                "{tool}\n\nUSAGE: {tool} [--check | --help]\n\n--check  Verify process startup; performs no data operations."
            );
            Ok(())
        }
        [arg] if arg == "--check" => {
            run(tool, async {
                tracing::info!(tool, "scaffold check passed; no data was read or written");
                Ok(())
            })
            .await
        }
        [] => anyhow::bail!(
            "{tool}: data operation is not implemented; use --check to verify startup"
        ),
        _ => anyhow::bail!("{tool}: unsupported arguments; use --help"),
    }
}

#[cfg(unix)]
async fn shutdown() -> std::io::Result<()> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = terminate.recv() => Ok(()),
    }
}

#[cfg(not(unix))]
async fn shutdown() -> std::io::Result<()> {
    tokio::signal::ctrl_c().await
}
