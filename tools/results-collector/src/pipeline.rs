//! Durable import/asset/outbox processor using existing service-only RPCs.
use crate::{
    Error, Result,
    job::{Job, JobFuture, JobProcessor, Lease, Phase},
    storage::{Store, immutable, immutable_file, publish_pointer},
};
use civpulse_results_source::{
    Download, Expectations, Limits, Prepared, Validators, prepare, stringify,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tempfile::TempDir;
use tokio::task::AbortHandle;
use uuid::Uuid;

pub struct Publisher {
    store: Arc<dyn Store>,
    election: Uuid,
    expectations: Expectations,
    limits: Limits,
    allowed_url: String,
    spool_root: PathBuf,
}
impl Publisher {
    /// Build only for enabled collection from explicitly reviewed environment configuration.
    /// # Errors
    /// Rejects missing identity, completeness bounds, and environment-crossing credentials.
    pub fn from_environment(mut value: impl FnMut(&str) -> Option<String>) -> Result<Self> {
        let mut required = |key| {
            value(key)
                .filter(|v| !v.is_empty())
                .ok_or(Error::Config("Missing publication configuration"))
        };
        let environment = required("CIVPULSE_ENV")?;
        let store = crate::storage::R2Store::new(
            &environment,
            &required("R2_ENDPOINT")?,
            &required("R2_BUCKET")?,
            &required("R2_PUBLIC_ORIGIN")?,
            &required("R2_ACCESS_KEY_ID")?,
            &required("R2_SECRET_ACCESS_KEY")?,
        )?;
        let election = Uuid::parse_str(&required("SOS_ELECTION_ID")?)
            .map_err(|_| Error::Config("Invalid reviewed election UUID"))?;
        let election_date = required("SOS_ELECTION_DATE")?;
        let allowed_url = required("SOS_SOURCE_URL")?;
        let jurisdictions = required("SOS_EXPECTED_JURISDICTIONS")?
            .parse::<usize>()
            .map_err(|_| Error::Config("Invalid reviewed jurisdiction count"))?;
        let items = required("SOS_EXPECTED_ITEMS")?
            .parse::<usize>()
            .map_err(|_| Error::Config("Invalid reviewed item count"))?;
        if jurisdictions == 0 || jurisdictions > 160 || items == 0 {
            return Err(Error::Config(
                "Reviewed completeness bounds must be positive",
            ));
        }
        let expectations = Expectations {
            election_date,
            expected_jurisdictions: Some(jurisdictions),
            expected_items: Some(items),
        };
        let spool_root =
            value("SOS_SPOOL_ROOT").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
        Ok(Self::new(
            Arc::new(store),
            election,
            expectations,
            allowed_url,
            spool_root,
        ))
    }

    #[must_use]
    pub fn new(
        store: Arc<dyn Store>,
        election: Uuid,
        expectations: Expectations,
        allowed_url: String,
        spool_root: PathBuf,
    ) -> Self {
        Self {
            store,
            election,
            expectations,
            limits: Limits::default(),
            allowed_url,
            spool_root,
        }
    }
    async fn process_job(&self, job: &Job, lease: Lease) -> Result<()> {
        if job.election_id != self.election
            || job.election_date != self.expectations.election_date
            || job.source_url != self.allowed_url
        {
            return Err(Error::Config(
                "Claimed job does not match reviewed source/date",
            ));
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut lost = lease.cancellation();
        let monitor_cancelled = cancelled.clone();
        let monitor = tokio::spawn(async move {
            if *lost.borrow() || lost.changed().await.is_err() || *lost.borrow() {
                monitor_cancelled.store(true, Ordering::Relaxed);
            }
        });
        let _cancellation = Cancellation {
            cancelled: cancelled.clone(),
            monitor: monitor.abort_handle(),
        };
        if matches!(job.phase, Phase::Import) && !self.import(job, &lease, &cancelled).await? {
            return Ok(());
        }
        self.publish_outbox(job, &lease).await
    }
    async fn import(&self, job: &Job, lease: &Lease, cancelled: &Arc<AtomicBool>) -> Result<bool> {
        tokio::fs::create_dir_all(&self.spool_root)
            .await
            .map_err(|_| Error::Storage("Cannot create spool root"))?;
        let spool = Arc::new(
            tempfile::Builder::new()
                .prefix("civpulse-results-")
                .tempdir_in(&self.spool_root)
                .map_err(|_| Error::Storage("Cannot create job spool"))?,
        );
        let path = spool.path().join("source.json");
        let Some(receipt) = self.acquire_receipt(job, lease, &path, cancelled).await? else {
            return Ok(false);
        };
        let prepared = prepare_blocking(
            path.clone(),
            spool.clone(),
            self.expectations.clone(),
            self.limits.clone(),
            cancelled.clone(),
        )
        .await?;
        if lease.is_lost() {
            return Err(Error::LeaseLost);
        }
        tracing::info!(event="source_validated",job_id=%job.job_id,election_id=%job.election_id,bytes=receipt["bytes"].as_u64(),source_sha256=receipt["sha256"].as_str(),jurisdictions=prepared.jurisdictions.len(),items=prepared.item_count,fingerprint=%prepared.fingerprint);
        immutable_file(
            self.store.as_ref(),
            receipt["raw_key"].as_str().ok_or(Error::InvalidResponse)?,
            &path,
            receipt["sha256"].as_str().ok_or(Error::InvalidResponse)?,
            receipt["bytes"].as_u64().ok_or(Error::InvalidResponse)?,
        )
        .await?;
        let begun:Value=lease.call("begin_sos_results_job",json!({"p_sha256":receipt["sha256"],"p_header":prepared.header,"p_jurisdictions":prepared.jurisdictions,"p_receipt":receipt})).await?;
        let snapshot = begun["snapshot_id"]
            .as_str()
            .and_then(|v| Uuid::parse_str(v).ok())
            .ok_or(Error::InvalidResponse)?;
        let published = begun["published"].as_bool().ok_or(Error::InvalidResponse)?;
        let manifest_key = format!(
            "elections/{}/revisions/{snapshot}/manifest.json",
            job.election_id
        );
        if !published {
            let durable_receipt = begun
                .get("receipt")
                .filter(|v| v.is_object())
                .ok_or(Error::InvalidResponse)?;
            if durable_receipt["sha256"] != receipt["sha256"]
                || durable_receipt["raw_key"] != receipt["raw_key"]
            {
                return Err(Error::Storage(
                    "Begin returned conflicting original receipt",
                ));
            }
            stage(&prepared, snapshot, lease, cancelled).await?;
            write_assets(
                self.store.as_ref(),
                &job.election_id.to_string(),
                &snapshot.to_string(),
                &prepared,
                durable_receipt,
                cancelled,
            )
            .await?;
        }
        lease.call::<Value>("publish_sos_results_job",json!({"p_snapshot_id":snapshot,"p_fingerprint":prepared.fingerprint,"p_manifest_key":manifest_key})).await?;
        tracing::info!(event="snapshot_committed",job_id=%job.job_id,snapshot_id=%snapshot,unchanged=published);
        if published {
            return Ok(false);
        }
        Ok(true)
    }
    async fn acquire_receipt(
        &self,
        job: &Job,
        lease: &Lease,
        path: &Path,
        cancelled: &AtomicBool,
    ) -> Result<Option<Value>> {
        let receipt = if job.snapshot_id.is_some() {
            let receipt = job.receipt.clone().ok_or(Error::InvalidResponse)?;
            let hash = receipt["sha256"].as_str().ok_or(Error::InvalidResponse)?;
            let key = format!("elections/{}/raw/{hash}.json", job.election_id);
            if receipt["raw_key"] != key
                || receipt["source_url"] != job.source_url
                || receipt["raw_url"] != self.store.url(&key)?
            {
                return Err(Error::Storage("Archived receipt identity mismatch"));
            }
            self.store
                .download(
                    &key,
                    path,
                    hash,
                    receipt["bytes"].as_u64().ok_or(Error::InvalidResponse)?,
                )
                .await?;
            receipt
        } else {
            match civpulse_results_source::download(
                &job.source_url,
                &self.allowed_url,
                path,
                &Validators {
                    etag: job.etag.clone(),
                    last_modified: job.last_modified.clone(),
                },
                &self.limits,
                cancelled,
            )
            .await
            .map_err(source_error)?
            {
                Download::Unchanged {
                    etag,
                    last_modified,
                    ..
                } => {
                    lease.call::<Value>("unchanged_sos_results_job",json!({"p_etag":etag.or_else(||job.etag.clone()),"p_last_modified":last_modified.or_else(||job.last_modified.clone())})).await?;
                    return Ok(None);
                }
                Download::Downloaded(downloaded) => {
                    let mut receipt =
                        serde_json::to_value(downloaded).map_err(|_| Error::InvalidResponse)?;
                    let key = format!(
                        "elections/{}/raw/{}.json",
                        job.election_id,
                        receipt["sha256"].as_str().ok_or(Error::InvalidResponse)?
                    );
                    receipt["source_url"] = json!(job.source_url);
                    receipt["raw_key"] = json!(key);
                    receipt["raw_url"] = json!(self.store.url(&key)?);
                    receipt
                }
            }
        };
        Ok(Some(receipt))
    }
    async fn publish_outbox(&self, job: &Job, lease: &Lease) -> Result<()> {
        let publication: Value = lease.call("confirm_sos_results_pointer", json!({})).await?;
        if publication.is_null() {
            return Ok(());
        }
        let manifest_key = publication["manifest_key"]
            .as_str()
            .ok_or(Error::InvalidResponse)?;
        let expected_key = format!(
            "elections/{}/revisions/{}/manifest.json",
            job.election_id,
            publication["snapshot_id"]
                .as_str()
                .ok_or(Error::InvalidResponse)?
        );
        if manifest_key != expected_key {
            return Err(Error::Storage("Committed manifest key mismatch"));
        }
        let manifest = self.store.get(manifest_key, 1024 * 1024).await?;
        if manifest.status != 200 {
            return Err(Error::Storage("Committed manifest unavailable"));
        }
        let mut pointer: Value = serde_json::from_slice(&manifest.body)
            .map_err(|_| Error::Storage("Invalid committed manifest"))?;
        if pointer["election_id"] != job.election_id.to_string()
            || pointer["snapshot_id"] != publication["snapshot_id"]
            || pointer["schema_version"] != 1
        {
            return Err(Error::Storage("Committed manifest identity mismatch"));
        }
        pointer["revision"] = publication["revision"].clone();
        pointer["published_at"] = publication["published_at"].clone();
        let revision = pointer["revision"].clone();
        let snapshot = pointer["snapshot_id"].clone();
        let status = publish_pointer(self.store.as_ref(), &pointer, || {
            let lease = lease.clone();
            let revision = revision.clone();
            let snapshot = snapshot.clone();
            async move {
                let current: Value = lease.call("confirm_sos_results_pointer", json!({})).await?;
                Ok(current["revision"] == revision && current["snapshot_id"] == snapshot)
            }
        })
        .await?;
        tracing::info!(event="pointer_published",job_id=%job.job_id,snapshot_id=snapshot.as_str(),revision=revision.as_str(),status);
        lease
            .call::<Value>(
                "complete_sos_results_pointer",
                json!({"p_revision":revision}),
            )
            .await?;
        Ok(())
    }
}
impl JobProcessor for Publisher {
    fn process<'a>(&'a self, job: &'a Job, lease: Lease) -> JobFuture<'a> {
        Box::pin(self.process_job(job, lease))
    }
}
struct Cancellation {
    cancelled: Arc<AtomicBool>,
    monitor: AbortHandle,
}
impl Drop for Cancellation {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.monitor.abort();
    }
}
async fn prepare_blocking(
    path: PathBuf,
    spool: Arc<TempDir>,
    expectations: Expectations,
    limits: Limits,
    cancelled: Arc<AtomicBool>,
) -> Result<Prepared> {
    // Blocking parser owns the spool until it stops, even if its async caller is cancelled.
    tokio::task::spawn_blocking(move || {
        prepare(
            &path,
            &spool.path().join("scopes"),
            &expectations,
            &limits,
            &cancelled,
        )
    })
    .await
    .map_err(|_| Error::Task)?
    .map_err(source_error)
}
#[allow(clippy::needless_pass_by_value)] // map_err consumes dependency errors; discard their sensitive chains.
fn source_error(error: civpulse_results_source::Error) -> Error {
    match error {
        civpulse_results_source::Error::Status {
            status,
            retry_after,
        } => Error::SourceHttp {
            status,
            retry_after: Some(u32::try_from(retry_after.clamp(1, 3600)).unwrap_or(3600)),
        },
        civpulse_results_source::Error::Cancelled => Error::Shutdown,
        _ => Error::Source("Source download or validation failed"),
    }
}
async fn scope(path: &Path) -> Result<Value> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|_| Error::Storage("Cannot read prepared scope"))?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err(Error::Storage("Prepared scope exceeds limit"));
    }
    serde_json::from_slice(&bytes).map_err(|_| Error::Storage("Invalid prepared scope"))
}
fn items(scope: &Value, index: usize) -> Result<Vec<Value>> {
    Ok(scope["ballotItems"]
        .as_array()
        .ok_or(Error::InvalidResponse)?
        .iter()
        .enumerate()
        .map(|(i, data)| json!({"jurisdiction_index":index,"item_index":i,"data":data}))
        .collect())
}
async fn stage(
    prepared: &Prepared,
    snapshot: Uuid,
    lease: &Lease,
    cancelled: &AtomicBool,
) -> Result<()> {
    for entry in &prepared.scopes {
        let scope = scope(&entry.path).await?;
        let mut batch = Vec::new();
        let mut size = 2;
        for item in items(&scope, entry.index)? {
            if cancelled.load(Ordering::Relaxed) {
                return Err(Error::LeaseLost);
            }
            let length = stringify(&item).map_err(source_error)?.len() + 1;
            if length > 2 * 1024 * 1024 {
                return Err(Error::Source("Item exceeds staging bound"));
            }
            if !batch.is_empty() && (batch.len() >= 50 || size + length > 2 * 1024 * 1024) {
                lease
                    .call::<Value>(
                        "stage_sos_results_job",
                        json!({"p_snapshot_id":snapshot,"p_items":batch}),
                    )
                    .await?;
                batch = Vec::new();
                size = 2;
            }
            batch.push(item);
            size += length;
        }
        if !batch.is_empty() {
            lease
                .call::<Value>(
                    "stage_sos_results_job",
                    json!({"p_snapshot_id":snapshot,"p_items":batch}),
                )
                .await?;
        }
    }
    Ok(())
}
/// Produce version-one immutable source projections in original source order.
/// # Errors
/// Rejects unsafe source paths, oversized summaries, missing evidence or failed asset verification.
pub async fn write_assets(
    store: &dyn Store,
    election: &str,
    snapshot: &str,
    prepared: &Prepared,
    receipt: &Value,
    cancelled: &AtomicBool,
) -> Result<Value> {
    let prefix = format!("elections/{election}/revisions/{snapshot}");
    let mut jurisdictions = Vec::new();
    for entry in &prepared.scopes {
        if cancelled.load(Ordering::Relaxed) {
            return Err(Error::LeaseLost);
        }
        let scope = scope(&entry.path).await?;
        let id = scope["id"].as_str().ok_or(Error::InvalidResponse)?;
        if id.is_empty()
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        {
            return Err(Error::Source("Unsupported jurisdiction path ID"));
        }
        let path = if entry.index == 0 {
            "state".into()
        } else {
            format!("counties/{id}")
        };
        let detail_key = format!("{prefix}/{path}-detail.json");
        let mut source_items = items(&scope, entry.index)?;
        let detail = json!({"schema_version":1,"election_id":election,"snapshot_id":snapshot,"jurisdiction":prepared.jurisdictions[entry.index],"items":source_items});
        immutable(
            store,
            &detail_key,
            &stringify(&detail).map_err(source_error)?,
        )
        .await?;
        for item in &mut source_items {
            let data = item["data"].as_object_mut().ok_or(Error::InvalidResponse)?;
            data.shift_remove("rankedChoiceResults");
            for option in data
                .get_mut("ballotOptions")
                .and_then(Value::as_array_mut)
                .ok_or(Error::InvalidResponse)?
            {
                let option = option.as_object_mut().ok_or(Error::InvalidResponse)?;
                option.shift_remove("precinctResults");
                option.shift_remove("groupResults");
            }
            item["detail_url"] = json!(store.url(&detail_key)?);
        }
        let summary = json!({"schema_version":1,"election_id":election,"snapshot_id":snapshot,"jurisdiction":prepared.jurisdictions[entry.index],"items":source_items});
        let bytes = stringify(&summary).map_err(source_error)?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(Error::Source("Compact scope exceeds 4 MiB"));
        }
        let key = format!("{prefix}/{path}.json");
        let hash = crate::storage::sha256(&bytes);
        immutable(store, &key, &bytes).await?;
        jurisdictions.push(json!({"id":scope["id"],"name":scope["name"],"item_count":scope["ballotItems"].as_array().ok_or(Error::InvalidResponse)?.len(),"url":store.url(&key)?,"sha256":hash,"detail_url":store.url(&detail_key)?}));
    }
    let manifest = json!({"schema_version":1,"election_id":election,"snapshot_id":snapshot,"source_url":receipt["source_url"],"source_created_at":prepared.header["createdAt"],"source_sha256":receipt["sha256"],"source_bytes":receipt["bytes"],"raw_url":receipt["raw_url"],"retrieved_at":receipt["retrieved_at"],"etag":receipt["etag"],"last_modified":receipt["last_modified"],"fingerprint":prepared.fingerprint,"jurisdictions":jurisdictions});
    let bytes = stringify(&manifest).map_err(source_error)?;
    // Reserve room for revision/published_at before activating a snapshot whose
    // pointer and manifest must fit the bounded operational/browser reader.
    if bytes.len() > crate::storage::MAX_POINTER - 1024 {
        return Err(Error::Source("Discovery manifest exceeds limit"));
    }
    immutable(store, &format!("{prefix}/manifest.json"), &bytes).await?;
    Ok(manifest)
}
