//! One read-only monitoring loop; the retained alert PVC stays mounted between observations.
use crate::{Error, Result};
use std::{
    future::Future,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const HEARTBEAT: &str = "/tmp/civpulse-results-monitor-heartbeat";
const PERIOD: Duration = Duration::from_secs(60);
const CYCLE_LIMIT: Duration = Duration::from_secs(100);
const MAX_HEARTBEAT_AGE: u64 = 180;

/// Run bounded, nonoverlapping notification passes until graceful shutdown.
/// Remote failures retry on a later pass; a stopped loop never leaves a live heartbeat writer.
/// Shutdown starts no further passes and lets the current bounded pass persist delivery state.
/// # Errors
/// Returns local heartbeat I/O errors. Individual observation/delivery failures are sanitized logs.
pub async fn run<F, Fut, S>(pass: F, shutdown: S, path: &Path) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<()>>,
    S: Future<Output = ()>,
{
    run_with_timing(pass, shutdown, path, PERIOD, CYCLE_LIMIT).await
}
async fn run_with_timing<F, Fut, S>(
    mut pass: F,
    shutdown: S,
    path: &Path,
    period: Duration,
    limit: Duration,
) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<()>>,
    S: Future<Output = ()>,
{
    tokio::pin!(shutdown);
    heartbeat(path).await?;
    loop {
        // Poll shutdown before constructing a pass: an already-requested stop
        // must not start new notification work. An active pass still drains below.
        tokio::select! {
            biased;
            () = &mut shutdown => return Ok(()),
            () = std::future::ready(()) => {},
        }
        let started = tokio::time::Instant::now();
        let cycle = tokio::time::timeout(limit, pass());
        tokio::pin!(cycle);
        let (result, stopping) = tokio::select! {
            biased;
            ()=&mut shutdown => (cycle.await,true),
            result=&mut cycle => (result,false),
        };
        tracing::info!(
            event = "notification_monitor_cycle",
            success = matches!(result, Ok(Ok(()))),
            timed_out = result.is_err()
        );
        heartbeat(path).await?;
        if stopping {
            return Ok(());
        }
        // Delay missed observations instead of replaying ticks or overlapping work.
        let next = started + period;
        tokio::select! {
            biased;
            ()=&mut shutdown => return Ok(()),
            ()=tokio::time::sleep_until(next)=>{},
        }
    }
}
async fn heartbeat(path: &Path) -> Result<()> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::LocalIo)?
        .as_secs();
    let temp = path.with_extension("tmp");
    tokio::fs::write(&temp, timestamp.to_string())
        .await
        .map_err(|_| Error::LocalIo)?;
    tokio::fs::rename(&temp, path)
        .await
        .map_err(|_| Error::LocalIo)
}
/// Local monitor-loop liveness, independent of database/provider health and source collection.
/// # Errors
/// Rejects missing, malformed, future or stale cycle heartbeats without network access.
pub async fn liveness() -> Result<()> {
    check_heartbeat(Path::new(HEARTBEAT)).await
}
async fn check_heartbeat(path: &Path) -> Result<()> {
    if tokio::fs::metadata(path)
        .await
        .map_err(|_| Error::LocalIo)?
        .len()
        > 32
    {
        return Err(Error::LocalIo);
    }
    let value = tokio::fs::read(path).await.map_err(|_| Error::LocalIo)?;
    let timestamp = std::str::from_utf8(&value)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or(Error::LocalIo)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::LocalIo)?
        .as_secs();
    if timestamp > now || now - timestamp > MAX_HEARTBEAT_AGE {
        return Err(Error::LocalIo);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    #[tokio::test]
    async fn pre_signaled_shutdown_never_constructs_a_pass() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("heartbeat");
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        stop_tx.send(()).unwrap();
        let calls = AtomicUsize::new(0);
        run_with_timing(
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Ok(()) }
            },
            async { stop_rx.await.unwrap() },
            &path,
            Duration::from_millis(2),
            Duration::from_millis(100),
        )
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn shutdown_drains_current_pass_and_never_starts_another() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("heartbeat");
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let mut stop = Some(stop_tx);
        let completed = Arc::new(AtomicUsize::new(0));
        let count = completed.clone();
        run_with_timing(
            move || {
                let tx = stop.take().expect("only one pass");
                let count = count.clone();
                async move {
                    tx.send(()).unwrap();
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
            async {
                stop_rx.await.unwrap();
            },
            &path,
            Duration::from_millis(2),
            Duration::from_millis(100),
        )
        .await
        .unwrap();
        assert_eq!(completed.load(Ordering::SeqCst), 1);
        check_heartbeat(&path).await.unwrap();
    }
    #[tokio::test]
    async fn failed_and_timed_out_passes_retry_without_overlap_and_shutdown_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("heartbeat");
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let mut stop = Some(stop_tx);
        let operation = run_with_timing(
            move || {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                let tx = if n == 2 { stop.take() } else { None };
                async move {
                    match n {
                        0 => Err(Error::Network),
                        1 => std::future::pending().await,
                        _ => {
                            tx.unwrap().send(()).unwrap();
                            Ok(())
                        }
                    }
                }
            },
            async {
                stop_rx.await.unwrap();
            },
            &path,
            Duration::from_millis(2),
            Duration::from_millis(10),
        );
        tokio::time::timeout(Duration::from_secs(1), operation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
    #[tokio::test]
    async fn heartbeat_rejects_stale_future_and_malformed_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("heartbeat");
        assert!(check_heartbeat(&path).await.is_err());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        for value in [
            (now - 181).to_string(),
            (now + 100).to_string(),
            "bad".into(),
            "1".repeat(100),
        ] {
            tokio::fs::write(&path, value).await.unwrap();
            assert!(check_heartbeat(&path).await.is_err());
        }
        heartbeat(&path).await.unwrap();
        check_heartbeat(&path).await.unwrap();
    }
}
