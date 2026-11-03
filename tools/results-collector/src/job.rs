//! Typed durable job envelope and fenced mutation interface.
use crate::{Error, Result, rpc::Rpc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{future::Future, pin::Pin};
use tokio::sync::watch;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Import,
    Pointer,
}
#[derive(Clone, Deserialize)]
pub struct Job {
    pub job_id: Uuid,
    pub lease_id: Uuid,
    pub election_id: Uuid,
    pub election_date: String,
    pub source_url: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub snapshot_id: Option<Uuid>,
    pub phase: Phase,
    pub revision: Option<String>,
    pub manifest_key: Option<String>,
    pub receipt: Option<Value>,
}
impl Job {
    #[must_use]
    pub fn lease_args(&self) -> Value {
        json!({"p_job_id": self.job_id, "p_lease_id": self.lease_id})
    }
}
#[derive(Clone)]
pub struct Lease {
    rpc: Rpc,
    job: Job,
    lost: watch::Receiver<bool>,
}
impl Lease {
    pub(crate) fn new(rpc: Rpc, job: Job, lost: watch::Receiver<bool>) -> Self {
        Self { rpc, job, lost }
    }
    #[must_use]
    pub fn is_lost(&self) -> bool {
        *self.lost.borrow()
    }
    /// Subscribe to lease cancellation. A closed channel also means stop.
    #[must_use]
    pub fn cancellation(&self) -> watch::Receiver<bool> {
        self.lost.clone()
    }
    /// Call a fenced mutation. All callers must use this instead of direct table writes.
    /// # Errors
    /// Rejects a lost lease or caller-supplied lease identity; propagates RPC failure.
    pub async fn call<T: serde::de::DeserializeOwned>(
        &self,
        name: &'static str,
        args: Value,
    ) -> Result<T> {
        if self.is_lost() {
            return Err(Error::LeaseLost);
        }
        let mut args = args.as_object().cloned().ok_or(Error::InvalidResponse)?;
        if args.contains_key("p_job_id") || args.contains_key("p_lease_id") {
            return Err(Error::Config("Lease identity is owned by worker"));
        }
        args.insert("p_job_id".into(), json!(self.job.job_id));
        args.insert("p_lease_id".into(), json!(self.job.lease_id));
        self.rpc.call(name, Value::Object(args)).await
    }
}
pub type JobFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
pub trait JobProcessor: Send + Sync + 'static {
    /// Process using the original receipt and fenced RPC interface; cancellation must be safe.
    fn process<'a>(&'a self, job: &'a Job, lease: Lease) -> JobFuture<'a>;
}
/// Safe placeholder until the validated publication pipeline is available.
pub struct UnsupportedProcessor;
impl JobProcessor for UnsupportedProcessor {
    fn process<'a>(&'a self, _: &'a Job, _: Lease) -> JobFuture<'a> {
        Box::pin(async { Err(Error::Unsupported) })
    }
}
