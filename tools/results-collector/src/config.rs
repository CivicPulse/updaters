//! Explicit configuration; secrets are never included in Debug output.
use crate::{Error, Result};
use std::{path::PathBuf, time::Duration};

#[derive(Clone)]
pub struct Config {
    pub environment: String,
    pub enabled: bool,
    pub concurrency: usize,
    pub heartbeat_path: PathBuf,
    pub rpc_url: Option<String>,
    pub rpc_key: Option<String>,
}
impl Config {
    /// Read environment values without mutating global process state.
    /// # Errors
    /// Rejects unknown environments, malformed enable flags and concurrency outside 1..=4.
    pub fn read(mut value: impl FnMut(&str) -> Option<String>) -> Result<Self> {
        let enabled = match value("SOS_COLLECTOR_ENABLED").as_deref() {
            None | Some("false") => false,
            Some("true") => true,
            _ => return Err(Error::Config("SOS_COLLECTOR_ENABLED must be true or false")),
        };
        let concurrency = value("SOS_COLLECTOR_CONCURRENCY").map_or(Ok(1), |n| {
            n.parse::<usize>()
                .map_err(|_| Error::Config("Invalid concurrency"))
        })?;
        if !(1..=4).contains(&concurrency) {
            return Err(Error::Config("Concurrency must be 1..=4"));
        }
        let environment = value("CIVPULSE_ENV").unwrap_or_else(|| "prod".to_owned());
        if !matches!(environment.as_str(), "dev" | "test" | "prod") {
            return Err(Error::Config("CIVPULSE_ENV must be dev, test or prod"));
        }
        Ok(Self {
            environment,
            enabled,
            concurrency,
            heartbeat_path: value("SOS_COLLECTOR_HEARTBEAT").map_or_else(
                || PathBuf::from("/tmp/civpulse-results-heartbeat"),
                PathBuf::from,
            ),
            rpc_url: value("SUPABASE_URL"),
            rpc_key: value("SUPABASE_SERVICE_ROLE_KEY"),
        })
    }
}
pub const RPC_DEADLINE: Duration = Duration::from_secs(60);
pub const RPC_RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
