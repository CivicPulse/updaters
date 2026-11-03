use civpulse_results_collector::{
    Error, Result, alerting,
    config::{Config, RPC_DEADLINE, RPC_RESPONSE_LIMIT},
    job::{JobProcessor, UnsupportedProcessor},
    monitoring,
    pipeline::Publisher,
    resources,
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
                "Usage: civpulse-results-collector [daemon|once|health|monitor|metrics|notify|alert-test|ntfy-test|resources|liveness|validate|--check|--help]",
            ));
        }
    };
    if matches!(command, "--help" | "-h") {
        println!(
            "civpulse-results-collector [daemon|once|health|monitor|metrics|notify|alert-test|ntfy-test|resources|liveness|validate|--check]\nCollection defaults to disabled. health checks service RPC access; liveness checks the local daemon heartbeat. --check performs no I/O. validate <path> <election-date> [expected-jurisdictions expected-items] validates local bytes without credentials."
        );
        return Ok(());
    }
    if command == "--check" {
        return Ok(());
    }
    if !matches!(
        command,
        "daemon"
            | "once"
            | "health"
            | "monitor"
            | "metrics"
            | "notify"
            | "alert-test"
            | "ntfy-test"
            | "resources"
            | "liveness"
    ) {
        return Err(Error::Config("Unknown collector command"));
    }
    if command == "resources" {
        return resources::command();
    }
    if matches!(command, "ntfy-test" | "alert-test") {
        let receipt = if command == "ntfy-test" {
            civpulse_results_collector::ntfy::verification().await?
        } else {
            alerting::verification().await?
        };
        println!(
            "{}",
            serde_json::to_string(&receipt).map_err(|_| Error::InvalidResponse)?
        );
        return Ok(());
    }
    let config = Config::read(|key| std::env::var(key).ok())?;
    if command == "liveness" {
        return worker::liveness(&config.heartbeat_path).await;
    }
    initialize_logging()?;
    let rpc = rpc_for_mode(&config, command)?;
    if command == "health" {
        return service_health(rpc.as_ref().ok_or(Error::Config("Missing health RPC"))?).await;
    }
    if matches!(command, "monitor" | "metrics" | "notify") {
        let rpc = rpc
            .as_ref()
            .ok_or(Error::Config("Missing operational RPC"))?;
        return operational_command(command, rpc).await;
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
    let processor: Arc<dyn JobProcessor> = if config.enabled {
        Arc::new(Publisher::from_environment(|key| std::env::var(key).ok())?)
    } else {
        Arc::new(UnsupportedProcessor)
    };
    let result = if command == "once" {
        let rpc = rpc.as_ref().ok_or(Error::Disabled)?;
        let once = worker::once(rpc, processor.clone(), &options);
        tokio::pin!(once);
        tokio::select! {
            result = &mut once => result.map(|_| ()),
            _ = shutdown_rx.changed() => tokio::time::timeout(options.drain_timeout, &mut once).await.map_err(|_| Error::Shutdown)?.map(|_| ()),
        }
    } else {
        worker::daemon(rpc, processor, options, shutdown_rx).await
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

fn initialize_logging() -> Result<()> {
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
    Ok(())
}

async fn operational_command(command: &str, rpc: &Rpc) -> Result<()> {
    if command == "notify" {
        let report = monitoring::observe(rpc)
            .await
            .unwrap_or_else(|_| monitoring::unavailable());
        // Each transport persists independently, even when its peer fails.
        let (email, push) = tokio::join!(
            alerting::command(&report),
            civpulse_results_collector::ntfy::command(&report)
        );
        monitoring::render(&report, monitoring::Format::Json)?;
        let outcome = email?;
        let push = push?;
        ensure_alert_coverage(&report, outcome, push)
    } else {
        monitoring::command(
            rpc,
            if command == "metrics" {
                monitoring::Format::Prometheus
            } else {
                monitoring::Format::Json
            },
        )
        .await
    }
}

fn ensure_alert_coverage(
    report: &monitoring::Report,
    email: alerting::Outcome,
    push: alerting::Outcome,
) -> Result<()> {
    if email == alerting::Outcome::Disabled
        && report.alerts.iter().any(|alert| {
            push == alerting::Outcome::Disabled
                || !civpulse_results_collector::ntfy::public_alert(alert)
        })
    {
        return Err(Error::Source(
            "Alerts disabled while monitoring has actionable conditions",
        ));
    }
    Ok(())
}

async fn service_health(rpc: &Rpc) -> Result<()> {
    let health: serde_json::Value = rpc
        .call("get_sos_collection_health", serde_json::json!({}))
        .await?;
    if !health.is_array() {
        return Err(Error::InvalidResponse);
    }
    println!(
        "{}",
        serde_json::to_string(&health).map_err(|_| Error::InvalidResponse)?
    );
    Ok(())
}

fn rpc_for_mode(config: &Config, command: &str) -> Result<Option<Rpc>> {
    let client = if config.enabled || matches!(command, "health" | "monitor" | "metrics" | "notify")
    {
        Some(Rpc::for_environment(
            &config.environment,
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
    Ok(client)
}

#[cfg(test)]
mod notification_coverage_tests {
    use super::*;
    use civpulse_results_collector::{
        monitoring::{Alert, AlertKind, Report},
        ntfy::PUBLIC_ELECTION,
    };
    use uuid::Uuid;

    fn report(ids: &[Uuid]) -> Report {
        Report {
            checked_at: chrono::Utc::now(),
            feeds: vec![],
            alerts: ids
                .iter()
                .map(|id| Alert {
                    election_id: *id,
                    kind: AlertKind::RepeatedErrors,
                })
                .collect(),
        }
    }

    #[test]
    fn enabled_public_push_cannot_cover_private_or_mixed_alerts_without_email() {
        let private = Uuid::from_u128(1);
        for ids in [
            vec![private],
            vec![PUBLIC_ELECTION, private],
            vec![Uuid::nil(), private],
        ] {
            assert!(
                ensure_alert_coverage(
                    &report(&ids),
                    alerting::Outcome::Disabled,
                    alerting::Outcome::Quiet
                )
                .is_err()
            );
        }
        for ids in [
            vec![],
            vec![PUBLIC_ELECTION],
            vec![Uuid::nil()],
            vec![PUBLIC_ELECTION, Uuid::nil()],
        ] {
            assert!(
                ensure_alert_coverage(
                    &report(&ids),
                    alerting::Outcome::Disabled,
                    alerting::Outcome::Quiet
                )
                .is_ok()
            );
        }
        assert!(
            ensure_alert_coverage(
                &report(&[private]),
                alerting::Outcome::Quiet,
                alerting::Outcome::Quiet
            )
            .is_ok()
        );
        assert!(
            ensure_alert_coverage(
                &report(&[PUBLIC_ELECTION]),
                alerting::Outcome::Disabled,
                alerting::Outcome::Disabled
            )
            .is_err()
        );
    }
}
