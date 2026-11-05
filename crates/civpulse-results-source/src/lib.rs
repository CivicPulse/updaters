//! Bounded Georgia SOS source download, validation and semantic fingerprints.
mod canonical;
mod download;
mod parse;
mod topology;
pub use topology::TopologyPolicy;

pub use canonical::{canonical, stringify, stringify_envelope};
pub use download::{Download, Receipt, Validators, download, retry_after};
pub use parse::{Expectations, Limits, Prepared, Scope, prepare};

/// Errors never retain source payloads, credentials, URLs, or HTTP bodies.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("source I/O failed")]
    Io(#[from] std::io::Error),
    #[error("invalid source JSON")]
    Json(#[from] serde_json::Error),
    #[error("source request failed")]
    Http(#[source] reqwest::Error),
    #[error("source HTTP {status}; retry after {retry_after} seconds")]
    Status { status: u16, retry_after: u64 },
    #[error("{0}")]
    Invalid(&'static str),
    #[error("operation cancelled or deadline exceeded")]
    Cancelled,
}

/// Shared crate result.
pub type Result<T> = std::result::Result<T, Error>;
