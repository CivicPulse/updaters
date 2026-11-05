use anyhow::{Context, Result};
use civpulse_webapi::{Backend, router};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [arg] if arg == "--help" => {
            println!(
                "civpulse-webapi [--check | --help]\nRequires SUPABASE_URL and SUPABASE_PUBLISHABLE_KEY; API_BIND defaults to 0.0.0.0:8080."
            );
            return Ok(());
        }
        [arg] if arg == "--check" => {
            // Offline container startup check, not a configuration/readiness check.
            println!("civpulse-webapi startup check passed");
            return Ok(());
        }
        [] => {}
        _ => anyhow::bail!("unsupported arguments; use --help"),
    }
    let filter = if std::env::var_os("RUST_LOG").is_some() {
        EnvFilter::try_from_default_env()?
    } else {
        EnvFilter::new("info")
    };
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(|error| anyhow::anyhow!("initialize logging: {error}"))?;
    let backend = Backend::new(
        &std::env::var("SUPABASE_URL").context("SUPABASE_URL is required")?,
        &std::env::var("SUPABASE_PUBLISHABLE_KEY")
            .context("SUPABASE_PUBLISHABLE_KEY is required")?,
    )?;
    let listener = tokio::net::TcpListener::bind(
        std::env::var("API_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into()),
    )
    .await?;
    tracing::info!(address = %listener.local_addr()?, "API listening");
    axum::serve(listener, router(backend))
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn shutdown() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                tokio::select! {
                    _ = signal.recv() => {}
                    _ = tokio::signal::ctrl_c() => {}
                }
            }
            Err(error) => tracing::error!(%error, "cannot install shutdown handler"),
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
