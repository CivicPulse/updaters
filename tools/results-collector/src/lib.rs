//! Durable Georgia SOS queue consumer. Production collection is disabled by default.
pub mod admission;
pub mod alerting;
pub mod config;
pub mod job;
pub mod monitoring;
pub mod pipeline;
pub mod resources;
pub mod rpc;
pub mod storage;
pub mod validate;
pub mod worker;

use std::fmt;

/// Bounded, sanitized failures safe for structured logs and durable job errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Config(&'static str),
    Network,
    Http(u16),
    Timeout,
    ResponseLimit,
    InvalidResponse,
    Disabled,
    Unsupported,
    LeaseLost,
    Shutdown,
    LocalIo,
    Task,
    Source(&'static str),
    Storage(&'static str),
    SourceHttp {
        status: u16,
        retry_after: Option<u32>,
    },
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(message) => write!(f, "Configuration: {message}"),
            Self::Http(status) => write!(f, "Results RPC HTTP {status}"),
            Self::Network => f.write_str("Results RPC transport failure"),
            Self::Timeout => f.write_str("Results RPC deadline exceeded"),
            Self::ResponseLimit => f.write_str("Results RPC response exceeds limit"),
            Self::InvalidResponse => f.write_str("Invalid results RPC response"),
            Self::Disabled => f.write_str("Collection disabled"),
            Self::Unsupported => {
                f.write_str("Collection pipeline not implemented; no publication attempted")
            }
            Self::LeaseLost => f.write_str("Collection lease lost"),
            Self::Shutdown => f.write_str("Collection interrupted; durable receipt will resume"),
            Self::LocalIo => f.write_str("Local heartbeat I/O failure"),
            Self::Source(message) => write!(f, "Source: {message}"),
            Self::Storage(message) => write!(f, "Storage: {message}"),
            Self::SourceHttp { status, .. } => write!(f, "Source HTTP {status}"),
            Self::Task => f.write_str("Worker task failed"),
        }
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;
