//! Queue orchestration. Timers only claim durable jobs; they never poll the SOS source.
use crate::{
    Error, Result,
    job::{Job, JobProcessor, Lease},
    rpc::Rpc,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::watch,
    task::JoinSet,
    time::{Instant, MissedTickBehavior},
};

#[derive(Clone)]
pub struct Options {
    pub enabled: bool,
    pub concurrency: usize,
    pub poll_interval: Duration,
    pub renew_interval: Duration,
    pub drain_timeout: Duration,
    pub heartbeat_path: Option<PathBuf>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            enabled: false,
            concurrency: 1,
            poll_interval: Duration::from_secs(15),
            renew_interval: Duration::from_secs(30),
            drain_timeout: Duration::from_secs(90),
            heartbeat_path: None,
        }
    }
}
impl Options {
    fn validate(&self) -> Result<()> {
        if !(1..=4).contains(&self.concurrency)
            || self.poll_interval.is_zero()
            || self.renew_interval.is_zero()
            || self.drain_timeout.is_zero()
        {
            return Err(Error::Config("Invalid worker bounds"));
        }
        Ok(())
    }
}
/// Claim at most one job. An empty queue is a successful check.
/// # Errors
/// Disabled collection, RPC failures, or failed/incomplete jobs return errors.
pub async fn once(rpc: &Rpc, processor: Arc<dyn JobProcessor>, options: &Options) -> Result<bool> {
    options.validate()?;
    if !options.enabled {
        return Err(Error::Disabled);
    }
    let job: Option<Job> = rpc.call("claim_sos_results_job", json!({})).await?;
    if let Some(job) = job {
        run_job(rpc.clone(), processor, job, options.renew_interval).await?;
        Ok(true)
    } else {
        Ok(false)
    }
}
/// Run until shutdown, then drain for a bounded period while retaining lease renewal.
/// # Errors
/// Returns configuration, heartbeat, or task failures. Individual failed jobs are durable retries.
pub async fn daemon(
    rpc: Option<Rpc>,
    processor: Arc<dyn JobProcessor>,
    options: Options,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    options.validate()?;
    if options.enabled && rpc.is_none() {
        return Err(Error::Config("Enabled worker requires RPC configuration"));
    }
    let mut tasks = JoinSet::new();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(1));
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut next_poll = Instant::now();
    loop {
        if *shutdown.borrow() {
            break;
        }
        tokio::select! {
            biased;
            _ = shutdown.changed() => { break; }
            _ = heartbeat.tick() => { write_heartbeat(options.heartbeat_path.as_deref()).await?; }
            finished = tasks.join_next(), if !tasks.is_empty() => { log_job(finished); }
            () = tokio::time::sleep_until(next_poll), if options.enabled && tasks.len() < options.concurrency => {
                let rpc = rpc.as_ref().ok_or(Error::Config("Missing RPC configuration"))?;
                // Interrupting an ambiguous claim leaves its durable lease to expire safely.
                let request = rpc.call::<Option<Job>>("claim_sos_results_job", json!({}));
                tokio::pin!(request);
                let claimed = loop {
                    tokio::select! {
                        biased;
                        _ = shutdown.changed() => { break None; }
                        _ = heartbeat.tick() => { write_heartbeat(options.heartbeat_path.as_deref()).await?; }
                        result = &mut request => { break Some(result); }
                    }
                };
                let Some(claimed) = claimed else { break; };
                match claimed {
                    Ok(Some(job)) => {
                        tasks.spawn(run_job(rpc.clone(), processor.clone(), job, options.renew_interval));
                        next_poll = Instant::now();
                    }
                    Ok(None) => { next_poll = Instant::now() + options.poll_interval; }
                    Err(error) => { tracing::error!(%error, event="claim_failure"); next_poll = Instant::now() + options.poll_interval; }
                }
            }
        }
    }
    let end = Instant::now() + options.drain_timeout;
    while !tasks.is_empty() {
        tokio::select! {
            () = tokio::time::sleep_until(end) => {
                // Drop cancellation-safe processors. Durable receipts and leases own replay.
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
                return Err(Error::Shutdown);
            }
            _ = heartbeat.tick() => { write_heartbeat(options.heartbeat_path.as_deref()).await?; }
            finished = tasks.join_next() => { log_job(finished); }
        }
    }
    Ok(())
}
fn log_job(finished: Option<std::result::Result<Result<()>, tokio::task::JoinError>>) {
    match finished {
        Some(Ok(Ok(()))) => tracing::info!(event = "job_completed"),
        Some(Ok(Err(error))) => tracing::error!(%error, event="job_failed"),
        Some(Err(_)) => tracing::error!(error=%Error::Task, event="job_failed"),
        None => {}
    }
}
async fn run_job(
    rpc: Rpc,
    processor: Arc<dyn JobProcessor>,
    job: Job,
    renewal_interval: Duration,
) -> Result<()> {
    let (lost_tx, lost_rx) = watch::channel(false);
    let lease = Lease::new(rpc.clone(), job.clone(), lost_rx);
    // The renewal future is independently scheduled even during asynchronous network I/O.
    // CPU parsing must run in a cancellation-aware blocking task in the later pipeline.
    let renewal_rpc = rpc.clone();
    let args = job.lease_args();
    let renewal_lost = lost_tx.clone();
    let mut renewal = tokio::spawn(async move {
        loop {
            tokio::time::sleep(renewal_interval).await;
            if !matches!(
                renewal_rpc
                    .call::<bool>("renew_sos_results_job", args.clone())
                    .await,
                Ok(true)
            ) {
                renewal_lost.send_replace(true);
                return Err(Error::LeaseLost);
            }
        }
    });
    let mut lifecycle = LeaseLifecycle {
        lost: lost_tx,
        renewal: renewal.abort_handle(),
    };
    let result = tokio::select! {
        biased;
        renewed = &mut renewal => renewed.unwrap_or(Err(Error::Task)),
        processed = processor.process(&job, lease.clone()) => processed,
    };
    // Record validity before revoking local processor handles. Failure reporting
    // uses the same database-fenced IDs, but retained processors must stop now.
    let report_failure = !lease.is_lost();
    lifecycle.stop();
    if let Err(error) = &result {
        if report_failure {
            let mut args = job.lease_args();
            args["p_error"] = json!(error.to_string());
            args["p_retry_after"] = match error {
                Error::SourceHttp {
                    retry_after: Some(seconds),
                    ..
                } => json!(seconds.clamp(&1, &3600)),
                _ => Value::Null,
            };
            // A failed failure-report is safe: durable lease expiry permits recovery.
            let _ = rpc.call::<Value>("fail_sos_results_job", args).await;
        }
    }
    result
}
struct LeaseLifecycle {
    lost: watch::Sender<bool>,
    renewal: tokio::task::AbortHandle,
}
impl LeaseLifecycle {
    fn stop(&mut self) {
        // Revoke synchronously: aborting renewal alone only schedules its drop.
        self.lost.send_replace(true);
        self.renewal.abort();
    }
}
impl Drop for LeaseLifecycle {
    fn drop(&mut self) {
        self.stop();
    }
}
async fn write_heartbeat(path: Option<&Path>) -> Result<()> {
    if let Some(path) = path {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::LocalIo)?
            .as_secs();
        let temporary = path.with_extension("tmp");
        tokio::fs::write(&temporary, seconds.to_string())
            .await
            .map_err(|_| Error::LocalIo)?;
        tokio::fs::rename(temporary, path)
            .await
            .map_err(|_| Error::LocalIo)?;
    }
    Ok(())
}
/// Local liveness only: performs no network operations and requires no credentials.
/// # Errors
/// Missing, malformed, future, or older-than-30-second heartbeats fail.
pub async fn liveness(path: &Path) -> Result<()> {
    let bytes = tokio::fs::read(path).await.map_err(|_| Error::LocalIo)?;
    if bytes.len() > 32 {
        return Err(Error::LocalIo);
    }
    let timestamp = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or(Error::LocalIo)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::LocalIo)?
        .as_secs();
    if timestamp > now || now - timestamp > 30 {
        return Err(Error::LocalIo);
    }
    Ok(())
}
