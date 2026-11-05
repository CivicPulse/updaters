//! Wire contracts shared by the durable collector and public results reader.
//!
//! Source data remains JSON to preserve unknown fields and missing/null/zero values.
use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// An arbitrary precision, unsigned decimal revision serialized as a JSON string.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Revision(String);

impl TryFrom<String> for Revision {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("revision must be a decimal string");
        }
        let normalized = value.trim_start_matches('0');
        Ok(Self(
            if normalized.is_empty() {
                "0"
            } else {
                normalized
            }
            .to_owned(),
        ))
    }
}

impl From<Revision> for String {
    fn from(value: Revision) -> Self {
        value.0
    }
}

impl Ord for Revision {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0
            .len()
            .cmp(&other.0.len())
            .then_with(|| self.0.cmp(&other.0))
    }
}

impl PartialOrd for Revision {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Public discovery document. Additional version-one provenance fields survive round trips.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pointer {
    pub schema_version: u32,
    pub election_id: String,
    pub snapshot_id: String,
    pub revision: Revision,
    pub jurisdictions: Vec<JurisdictionLink>,
    #[serde(flatten)]
    pub provenance: Map<String, Value>,
}

/// Immutable scope references inside a discovery document.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JurisdictionLink {
    pub id: String,
    pub name: String,
    pub item_count: usize,
    pub url: String,
    pub sha256: String,
    pub detail_url: String,
}

/// Immutable source projection; curated identities must never be added here.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScopeAsset {
    pub schema_version: u32,
    pub election_id: String,
    pub snapshot_id: String,
    pub jurisdiction: Map<String, Value>,
    pub items: Vec<SourceItem>,
}

/// Source indexes are positions in the original import, not curated identities.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceItem {
    pub jurisdiction_index: usize,
    pub item_index: usize,
    pub data: Map<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail_url: Option<String>,
}
