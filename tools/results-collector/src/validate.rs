//! Credential-free local source validation with cooperative blocking-task shutdown.
use std::{
    future::Future,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use civpulse_results_source::{Expectations, Limits, prepare};
use serde::Serialize;

use crate::{Error, Result};

/// Local validation settings; no environment or credentials are read.
pub struct ValidationOptions {
    path: PathBuf,
    expectations: Expectations,
}
impl ValidationOptions {
    /// Parse `<path> <election-date> [expected-jurisdictions expected-items]`.
    /// # Errors
    /// Rejects malformed argument counts or invalid completeness counts.
    pub fn parse(args: &[String]) -> Result<Self> {
        let (path, date, counts) = match args {
            [path, date] => (path, date, None),
            [path, date, scopes, items] => (path, date, Some((scopes, items))),
            _ => {
                return Err(Error::Config(
                    "Usage: civpulse-results-collector validate <path> <election-date> [expected-jurisdictions expected-items]",
                ));
            }
        };
        if path.is_empty() || date.is_empty() {
            return Err(Error::Config(
                "Validation path and election date are required",
            ));
        }
        let (expected_jurisdictions, expected_items) = if let Some((scopes, items)) = counts {
            let scopes = scopes
                .parse::<usize>()
                .ok()
                .filter(|count| (1..=160).contains(count))
                .ok_or(Error::Config("Expected jurisdictions must be 1..=160"))?;
            let items = items.parse::<usize>().ok().ok_or(Error::Config(
                "Expected items must be a nonnegative integer",
            ))?;
            (Some(scopes), Some(items))
        } else {
            (None, None)
        };
        Ok(Self {
            path: PathBuf::from(path),
            expectations: Expectations {
                election_date: date.clone(),
                expected_jurisdictions,
                expected_items,
                accepted_topologies: vec![],
                topology_policy: None,
            },
        })
    }
}

/// Sanitized successful validation output. It contains no source contents or paths.
#[derive(Debug, Serialize)]
pub struct ValidationSummary {
    pub event: &'static str,
    pub item_count: usize,
    pub jurisdictions: usize,
    pub fingerprint: String,
    pub topology_sha256: String,
    pub duration_ms: u128,
}

/// Validate locally and join the parser after shutdown instead of abandoning it.
///
/// The blocking task owns its temporary directory. A shutdown signal sets the
/// parser's cancellation flag; returning waits for cleanup and parser termination.
/// # Errors
/// Returns sanitized local/validation errors or interrupted validation on shutdown.
pub async fn validate(
    options: ValidationOptions,
    shutdown: impl Future<Output = std::io::Result<()>>,
) -> Result<ValidationSummary> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let parse_cancelled = cancelled.clone();
    let mut task = tokio::task::spawn_blocking(move || {
        let temporary = tempfile::tempdir().map_err(|_| Error::LocalIo)?;
        let start = Instant::now();
        let prepared = prepare(
            &options.path,
            &temporary.path().join("scopes"),
            &options.expectations,
            &Limits::default(),
            &parse_cancelled,
        )
        .map_err(|_| Error::Source("Local source validation failed"))?;
        Ok(ValidationSummary {
            event: "source_validated",
            item_count: prepared.item_count,
            jurisdictions: prepared.scopes.len(),
            fingerprint: prepared.fingerprint,
            topology_sha256: prepared.topology_sha256,
            duration_ms: start.elapsed().as_millis(),
        })
    });
    tokio::select! {
        biased;
        signal = shutdown => {
            cancelled.store(true,Ordering::Relaxed);
            // spawn_blocking cannot be aborted once running. Await its bounded
            // cooperative cancellation and TempDir cleanup before returning.
            let _ = task.await.map_err(|_|Error::Task)?;
            signal.map_err(|_|Error::LocalIo)?;
            Err(Error::Shutdown)
        }
        result = &mut task => result.map_err(|_|Error::Task)?,
    }
}

/// Parse local validation arguments and print one sanitized JSON result.
/// # Errors
/// Returns argument, validation, shutdown or output serialization failures.
pub async fn run(
    args: &[String],
    shutdown: impl Future<Output = std::io::Result<()>>,
) -> Result<()> {
    let summary = validate(ValidationOptions::parse(args)?, shutdown).await?;
    println!(
        "{}",
        serde_json::to_string(&summary).map_err(|_| Error::InvalidResponse)?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn requested_shutdown_cancels_and_joins_local_validation() {
        let options =
            ValidationOptions::parse(&["unused-local-source".to_owned(), "2026-11-03".to_owned()])
                .unwrap();
        // Bias toward an immediate shutdown without relying on parser timing.
        let result = validate(options, std::future::ready(Ok(()))).await;
        assert!(matches!(result, Err(Error::Shutdown)));
    }
}
