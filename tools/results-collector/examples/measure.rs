//! Offline resource measurement using the real Publisher and fenced queue lifecycle.
#[path = "support/disk.rs"]
mod disk;
#[path = "support/rpc.rs"]
mod local_rpc;
use civpulse_results_collector::{
    Error, Result,
    pipeline::Publisher,
    rpc::Rpc,
    storage::{Conditions, Store},
    worker::{self, Options},
};
use civpulse_results_source::{Expectations, Limits, prepare};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

#[tokio::main(worker_threads = 2)]
async fn main() -> Result<()> {
    let mut args: Vec<_> = std::env::args().skip(1).collect();
    let explicit_root =
        if args.first().is_some_and(|arg| arg == "--scratch-root") && args.len() >= 2 {
            let root = PathBuf::from(&args[1]);
            args.drain(..2);
            Some(root)
        } else {
            None
        };
    let (scratch, filesystem) = scratch_directory(explicit_root)?;
    if args.len() < 2 || args.len() > 5 {
        return Err(Error::Config(
            "Usage: measure INPUT DATE [--pipeline] [JURISDICTIONS ITEMS]",
        ));
    }
    let stdin_file = if args[0] == "-" {
        Some(receive_stdin(scratch.path())?)
    } else {
        None
    };
    let source = stdin_file
        .as_ref()
        .map_or_else(|| PathBuf::from(&args[0]), |file| file.path().to_path_buf());
    let (pipeline, expectations, jurisdictions, items) = parse_expectations(&args)?;
    let peak = Arc::new(Peaks::default());
    let done = Arc::new(AtomicBool::new(false));
    let _done_guard = DoneOnDrop(done.clone());
    let peak_task = tracker(scratch.path().to_path_buf(), peak.clone(), done.clone());
    let start = Instant::now();
    let (hash, bytes) = disk::digest(&source).await?;
    let hash_ms = start.elapsed().as_millis();
    let validation_start = Instant::now();
    let input = source.clone();
    let out = scratch.path().join("validation");
    let expected = expectations.clone();
    let admission =
        civpulse_results_collector::admission::Admission::read(|key| std::env::var(key).ok())?;
    let limits = Limits {
        max_download: admission.download_bytes,
        max_scope: admission.scope_bytes,
        ..Limits::default()
    };
    let prepared = tokio::task::spawn_blocking(move || {
        prepare(&input, &out, &expected, &limits, &AtomicBool::new(false))
    })
    .await
    .map_err(|_| Error::Task)?
    .map_err(|_| Error::Source("Offline validation failed"))?;
    let validation_ms = validation_start.elapsed().as_millis();
    let fingerprint = prepared.fingerprint.clone();
    drop(prepared);
    // This initial validation spool is not retained while measuring pipeline disk use.
    tokio::fs::remove_dir_all(scratch.path().join("validation"))
        .await
        .map_err(|_| Error::LocalIo)?;
    let metrics = if pipeline {
        Some(
            run_pipeline(
                &source,
                scratch.path(),
                expectations,
                &hash,
                bytes,
                &fingerprint,
            )
            .await?,
        )
    } else {
        None
    };
    done.store(true, Ordering::Relaxed);
    peak_task.await.map_err(|_| Error::Task)?;
    sample(scratch.path(), &peak);
    let status = std::fs::read_to_string("/proc/self/status").map_err(|_| Error::LocalIo)?;
    let peak_rss = status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|v| v.parse::<u64>().ok())
        .map(|kb| kb * 1024);
    println!(
        "{}",
        json!({"measurement":"local_offline_simulation","code_base_revision":option_env!("CIVPULSE_MEASURE_CODE_REVISION").unwrap_or("unrecorded"),"measurement_harness_sha256":harness_hash(),"source_bytes":bytes,"raw_sha256":hash,"semantic_sha256":fingerprint,"jurisdictions":jurisdictions,"items":items,"raw_hash_ms":hash_ms,"validation_ms":validation_ms,"simulated_pipeline_ms":metrics.as_ref().map(|m|m.duration_ms),"duration_ms":start.elapsed().as_millis(),"staged_items":metrics.as_ref().map(|m|m.staged),"staged_batches":metrics.as_ref().map(|m|m.batches),"largest_rpc_batch_bytes":metrics.as_ref().map(|m|m.largest_batch),"peak_rss_bytes":peak_rss,"sampled_peak_total_local_bytes":peak.total.load(Ordering::Relaxed),"sampled_peak_worker_scratch_bytes":peak.worker.load(Ordering::Relaxed),"sampled_peak_simulated_object_bytes":peak.objects.load(Ordering::Relaxed),"scratch_sample_interval_ms":10,"scratch_excludes_input":true,"scratch_filesystem":filesystem,"platform":format!("{}-{}",std::env::consts::OS,std::env::consts::ARCH),"production_acceptance":false,"container_resources":civpulse_results_collector::resources::read()})
    );
    Ok(())
}
fn parse_expectations(args: &[String]) -> Result<(bool, Expectations, usize, usize)> {
    let (pipeline, counts) = match &args[2..] {
        [] => (false, None),
        [flag] if flag == "--pipeline" => (true, None),
        [jurisdictions, items] => (false, Some((jurisdictions, items))),
        [flag, jurisdictions, items] if flag == "--pipeline" => {
            (true, Some((jurisdictions, items)))
        }
        _ => return Err(Error::Config("Supply both jurisdiction and item counts")),
    };
    let (jurisdictions, items) = if let Some((jurisdictions, items)) = counts {
        let jurisdictions = jurisdictions
            .parse::<usize>()
            .ok()
            .filter(|count| (1..=160).contains(count))
            .ok_or(Error::Config("Invalid jurisdiction count"))?;
        let items = items
            .parse::<usize>()
            .map_err(|_| Error::Config("Invalid item count"))?;
        (jurisdictions, items)
    } else {
        (160, 4_178)
    };
    let expectations = Expectations {
        election_date: args[1].clone(),
        expected_jurisdictions: Some(jurisdictions),
        expected_items: Some(items),
        accepted_topologies: vec![],
        topology_policy: None,
    };
    Ok((pipeline, expectations, jurisdictions, items))
}
fn tracker(path: PathBuf, peak: Arc<Peaks>, done: Arc<AtomicBool>) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        while !done.load(Ordering::Relaxed) {
            sample(&path, &peak);
            std::thread::sleep(Duration::from_millis(10));
        }
    })
}
fn disk_bytes(path: &Path) -> u64 {
    std::fs::read_dir(path).map_or(0, |entries| {
        entries
            .filter_map(std::result::Result::ok)
            .map(|entry| {
                entry.metadata().map_or(0, |m| {
                    if m.is_dir() {
                        disk_bytes(&entry.path())
                    } else {
                        m.len()
                    }
                })
            })
            .sum()
    })
}

struct DoneOnDrop(Arc<AtomicBool>);
impl Drop for DoneOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

struct PipelineMetrics {
    staged: usize,
    batches: usize,
    largest_batch: usize,
    duration_ms: u128,
}
async fn run_pipeline(
    source: &Path,
    scratch: &Path,
    expectations: Expectations,
    hash: &str,
    bytes: u64,
    fingerprint: &str,
) -> Result<PipelineMetrics> {
    let expected_items = expectations
        .expected_items
        .ok_or(Error::Config("Expected items required"))?;
    let beginning = Instant::now();
    let store = Arc::new(disk::DiskStore::new(scratch.join("objects")));
    let raw_key = format!("elections/{}/raw/{hash}.json", local_rpc::ELECTION);
    // Stream local bytes into the simulated archive, then exercise original-receipt resume.
    store
        .put_file(
            &raw_key,
            source,
            hash,
            bytes,
            Conditions {
                if_none_match: true,
                ..Conditions::default()
            },
        )
        .await?;
    let receipt = json!({"source_url":local_rpc::SOURCE,"raw_key":raw_key,"raw_url":store.url(&raw_key)?,"sha256":hash,"bytes":bytes,"retrieved_at":"2026-11-03T10:00:10Z","etag":null,"last_modified":null});
    let state = Arc::new(Mutex::new(local_rpc::State::default()));
    let (url, server) =
        local_rpc::start(receipt, expectations.election_date.clone(), state.clone()).await?;
    let publisher = Publisher::new(
        store.clone(),
        uuid::Uuid::parse_str(local_rpc::ELECTION).map_err(|_| Error::InvalidResponse)?,
        expectations,
        local_rpc::SOURCE.into(),
        scratch.join("spool"),
    )
    .with_admission(civpulse_results_collector::admission::Admission::read(
        |key| std::env::var(key).ok(),
    )?)?;
    let rpc = Rpc::new(
        &url,
        "offline-benchmark-not-a-secret",
        Duration::from_secs(60),
        4 * 1024 * 1024,
    )?;
    let result = worker::once(
        &rpc,
        Arc::new(publisher),
        &Options {
            enabled: true,
            ..Options::default()
        },
    )
    .await;
    server.abort();
    if !result? {
        return Err(Error::Storage("Offline queue unexpectedly empty"));
    }
    let s = state.lock().await;
    if !s.completed || s.staged != expected_items || s.fingerprint != fingerprint {
        return Err(Error::Storage("Offline pipeline completeness mismatch"));
    }
    let pointer = store
        .get(
            &format!("elections/{}/latest.json", local_rpc::ELECTION),
            1024 * 1024,
        )
        .await?;
    let pointer: serde_json::Value =
        serde_json::from_slice(&pointer.body).map_err(|_| Error::InvalidResponse)?;
    if pointer["revision"] != "9007199254740993" || pointer["snapshot_id"] != local_rpc::SNAPSHOT {
        return Err(Error::Storage("Offline pointer readback mismatch"));
    }
    Ok(PipelineMetrics {
        staged: s.staged,
        batches: s.batches,
        largest_batch: s.largest_batch,
        duration_ms: beginning.elapsed().as_millis(),
    })
}

fn scratch_directory(explicit: Option<PathBuf>) -> Result<(tempfile::TempDir, String)> {
    let scratch_root = explicit
        .or_else(|| std::env::var_os("CIVPULSE_MEASURE_SCRATCH_ROOT").map(PathBuf::from))
        .ok_or(Error::Config(
            "Set CIVPULSE_MEASURE_SCRATCH_ROOT to an existing disk-backed directory",
        ))?;
    let root = scratch_root.canonicalize().map_err(|_| Error::LocalIo)?;
    let mounts = std::fs::read_to_string("/proc/self/mountinfo").map_err(|_| Error::LocalIo)?;
    let filesystem = mounts
        .lines()
        .filter_map(|line| {
            let (mount, details) = line.split_once(" - ")?;
            let path = mount.split_whitespace().nth(4)?;
            // Decode the kernel's mountinfo escapes before matching path components.
            let path = path
                .replace("\\040", " ")
                .replace("\\011", "\t")
                .replace("\\012", "\n")
                .replace("\\134", "\\");
            root.starts_with(&path)
                .then(|| (path.len(), details.split_whitespace().next().unwrap_or("")))
        })
        .max_by_key(|(length, _)| *length)
        .map(|(_, kind)| kind.to_owned())
        .ok_or(Error::LocalIo)?;
    if ["tmpfs", "ramfs"].contains(&filesystem.as_str()) {
        return Err(Error::Config(
            "Resource measurement scratch must be disk-backed",
        ));
    }
    let scratch = tempfile::Builder::new()
        .prefix("civpulse-offline-measure-")
        .tempdir_in(&scratch_root)
        .map_err(|_| Error::LocalIo)?;
    Ok((scratch, filesystem))
}

#[derive(Default)]
struct Peaks {
    total: AtomicU64,
    worker: AtomicU64,
    objects: AtomicU64,
}
fn sample(path: &Path, peak: &Peaks) {
    let worker = disk_bytes(&path.join("validation")) + disk_bytes(&path.join("spool"));
    let objects = disk_bytes(&path.join("objects"));
    peak.total.fetch_max(worker + objects, Ordering::Relaxed);
    peak.worker.fetch_max(worker, Ordering::Relaxed);
    peak.objects.fetch_max(objects, Ordering::Relaxed);
}

fn harness_hash() -> String {
    let mut hash = Sha256::new();
    for bytes in [
        include_bytes!("measure.rs").as_slice(),
        include_bytes!("support/disk.rs").as_slice(),
        include_bytes!("support/rpc.rs").as_slice(),
    ] {
        hash.update(bytes.len().to_le_bytes());
        hash.update(bytes);
    }
    hex::encode(hash.finalize())
}

fn receive_stdin(root: &Path) -> Result<tempfile::NamedTempFile> {
    use std::io::Read;
    let admission =
        civpulse_results_collector::admission::Admission::read(|key| std::env::var(key).ok())?;
    let mut file = tempfile::Builder::new()
        .prefix("civpulse-results-resource-input-")
        .tempfile_in(root)
        .map_err(|_| Error::LocalIo)?;
    let bytes = std::io::copy(
        &mut std::io::stdin().lock().take(admission.download_bytes + 1),
        &mut file,
    )
    .map_err(|_| Error::LocalIo)?;
    if bytes == 0 || bytes > admission.download_bytes {
        return Err(Error::Source("Resource input outside configured admission"));
    }
    Ok(file)
}
