//! Shared permission-preserving civic operations for REST and MCP.
pub mod backend;
pub mod catalog;
mod georgia;
pub mod handlers;
mod meeting_source;
pub mod resources;
mod simbli;
mod uploads;

#[cfg(test)]
#[path = "../tests/support/compatibility.rs"]
mod compatibility;
