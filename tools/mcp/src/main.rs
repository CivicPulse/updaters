use anyhow::{Result, bail};
use civpulse_mcp::server::{Config, router};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["--help" | "-h"] => {
            println!(
                "civpulse-mcp [--check | --help]\n\nServe the Civic Pulse V2 catalog over HTTP.\nRequired: SUPABASE_URL, SUPABASE_ANON_KEY, MCP_RESOURCE_URL, AUTHORIZATION_SERVER\nOptional: MCP_LISTEN (0.0.0.0:8789), KB_API_URL, GEORGIA_SOURCE_READER_URL\n--check validates the embedded catalog without contacting external services."
            );
            return Ok(());
        }
        ["--check"] => {
            for tool in civpulse_mcp::catalog::tools() {
                jsonschema::validator_for(&tool["inputSchema"])?;
            }
            println!(
                "Civic Pulse V2: {} tool schemas valid",
                civpulse_mcp::catalog::tools().len()
            );
            return Ok(());
        }
        [] => {}
        _ => bail!("Unsupported arguments; use --help"),
    }
    tracing_subscriber::fmt()
        .json()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let config = Config::from_env()?;
    let listener = tokio::net::TcpListener::bind(&config.listen).await?;
    tracing::info!(address=%listener.local_addr()?,"Civic Pulse MCP listening");
    axum::serve(listener, router(config)?)
        .with_graceful_shutdown(async {
            #[cfg(unix)]
            {
                let mut term =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("SIGTERM handler");
                tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }
        })
        .await?;
    Ok(())
}
