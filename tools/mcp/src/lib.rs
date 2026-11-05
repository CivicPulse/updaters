//! Civic Pulse V2 MCP server. Every backend call uses the authenticated caller's token.
mod backend;
pub mod catalog;
mod georgia;
mod handlers;
mod meeting_source;
pub mod server;
mod simbli;
mod uploads;

#[cfg(test)]
#[path = "../tests/support/compatibility.rs"]
mod compatibility;
