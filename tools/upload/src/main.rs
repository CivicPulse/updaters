//! Starter for independent `CivPulse` upload jobs.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    civpulse_runtime::scaffold(env!("CARGO_PKG_NAME")).await
}
