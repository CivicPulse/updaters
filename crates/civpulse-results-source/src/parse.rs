use std::{
    collections::HashSet,
    fs::{self, File},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use chrono::{DateTime, NaiveDate, Utc};
use serde::{
    Deserialize,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::{
    Error, Result,
    canonical::{collator, write_canonical},
};

/// Hard resource caps. Defaults match the reviewed upstream contract.
#[derive(Clone, Debug)]
pub struct Limits {
    pub max_download: u64,
    pub max_scope: usize,
    pub max_header: usize,
    pub max_metadata: usize,
    pub max_jurisdictions: usize,
    pub max_batch_bytes: usize,
    pub max_batch_items: usize,
    pub download_timeout: Duration,
    pub parse_timeout: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_download: 512 * 1024 * 1024,
            max_scope: 64 * 1024 * 1024,
            max_header: 1024 * 1024,
            max_metadata: 128 * 1024,
            max_jurisdictions: 160,
            max_batch_bytes: 2 * 1024 * 1024,
            max_batch_items: 50,
            download_timeout: Duration::from_secs(180),
            parse_timeout: Duration::from_secs(180),
        }
    }
}

/// Reviewed feed identity and completeness expectations; never infer an election UUID.
#[derive(Clone, Debug)]
pub struct Expectations {
    pub election_date: String,
    pub expected_jurisdictions: Option<usize>,
    pub expected_items: Option<usize>,
    /// Optional reviewed identity-membership hashes; replaces only exact item-count equality.
    pub accepted_topologies: Vec<String>,
}

impl Expectations {
    /// Reject malformed, duplicated or excessive reviewed membership hashes before I/O.
    /// # Errors
    /// Returns an error for an invalid allowlist.
    pub fn validate_topologies(&self) -> Result<()> {
        if self.accepted_topologies.len() > 8
            || self.accepted_topologies.iter().any(|hash| {
                hash.len() != 64
                    || !hash
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            || self
                .accepted_topologies
                .iter()
                .collect::<HashSet<_>>()
                .len()
                != self.accepted_topologies.len()
        {
            return Err(Error::Invalid("invalid reviewed topology allowlist"));
        }
        Ok(())
    }
}

/// One validated spool scope. Its source order index is stable across retries.
#[derive(Clone, Debug)]
pub struct Scope {
    pub index: usize,
    pub path: PathBuf,
    pub metadata: Value,
}

/// Validated bounded spool plus the reference-compatible semantic fingerprint.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub header: Map<String, Value>,
    pub jurisdictions: Vec<Value>,
    pub scopes: Vec<Scope>,
    pub fingerprint: String,
    pub topology_sha256: String,
    pub item_count: usize,
}

/// Parse incrementally, holding at most one bounded jurisdiction at a time.
///
/// `directory` must not exist; it is removed on failure. Successful spool files
/// belong to the caller. This synchronous function checks cancellation/deadlines
/// cooperatively; call on a blocking thread, cancel and join it during shutdown.
/// It performs no network, database, or public object writes.
///
/// # Errors
/// Rejects malformed/duplicate JSON, invalid identity/counts/IDs, size/deadline
/// violations, and mismatches against reviewed completeness expectations.
#[allow(clippy::too_many_lines)] // Keep the ordered root state machine visible for contract review.
pub fn prepare(
    path: &Path,
    directory: &Path,
    expectations: &Expectations,
    limits: &Limits,
    cancelled: &AtomicBool,
) -> Result<Prepared> {
    if limits.max_download == 0
        || limits.max_scope == 0
        || limits.max_batch_items == 0
        || limits.max_jurisdictions == 0
    {
        return Err(Error::Invalid("invalid zero resource limit"));
    }
    expectations.validate_topologies()?;
    let date = NaiveDate::parse_from_str(&expectations.election_date, "%Y-%m-%d")
        .map_err(|_| Error::Invalid("invalid expected election date"))?;
    if date.format("%Y-%m-%d").to_string() != expectations.election_date {
        return Err(Error::Invalid("invalid expected election date"));
    }
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(Error::Invalid("source must be a regular local file"));
    }
    if metadata.len() > limits.max_download {
        return Err(Error::Invalid("source download exceeds limit"));
    }
    fs::create_dir(directory)?;
    let mut cleanup = Cleanup {
        path: directory,
        keep: false,
    };
    let deadline = Instant::now()
        .checked_add(limits.parse_timeout)
        .ok_or(Error::Invalid("invalid parse deadline"))?;
    let mut reader = Reader {
        buffer: BufReader::with_capacity(
            64 * 1024,
            File::open(path)?.take(limits.max_download.saturating_add(1)),
        ),
        read: 0,
        limits,
        cancelled,
        deadline,
    };
    // A BOM is permitted only at the start; it remains untouched in the archive.
    if reader.peek_raw()? == Some(0xef) {
        for byte in [0xef, 0xbb, 0xbf] {
            if reader.next()? != Some(byte) {
                return Err(Error::Invalid("invalid UTF-8 BOM"));
            }
        }
    }
    reader.token(b'{')?;
    let mut keys = HashSet::new();
    let mut header = Map::new();
    let mut scopes = Vec::new();
    let mut scope_ids = HashSet::new();
    let mut hashes = Vec::new();
    let mut topology = Vec::new();
    let mut item_count = 0_usize;
    let mut county_count = 0_usize;
    let collator = collator()?;
    while reader.peek()? != Some(b'}') {
        let key = reader.value(limits.max_header)?;
        let key = key
            .as_str()
            .ok_or(Error::Invalid("invalid root key"))?
            .to_owned();
        if !keys.insert(key.clone()) {
            return Err(Error::Invalid("duplicate root key"));
        }
        reader.token(b':')?;
        if key == "localResults" {
            reader.token(b'[')?;
            while reader.peek()? != Some(b']') {
                county_count += 1;
                if county_count >= limits.max_jurisdictions {
                    return Err(Error::Invalid("too many jurisdictions"));
                }
                let scope = reader.value(limits.max_scope)?;
                process_scope(
                    scope,
                    county_count,
                    directory,
                    limits,
                    &mut scopes,
                    &mut scope_ids,
                    &mut hashes,
                    &mut topology,
                    &mut item_count,
                    &reader,
                    &collator,
                )?;
                if reader.peek()? == Some(b']') {
                    break;
                }
                reader.token(b',')?;
                if reader.peek()? == Some(b']') {
                    return Err(Error::Invalid("trailing array comma"));
                }
            }
            reader.token(b']')?;
        } else if key == "results" {
            let scope = reader.value(limits.max_scope)?;
            process_scope(
                scope,
                0,
                directory,
                limits,
                &mut scopes,
                &mut scope_ids,
                &mut hashes,
                &mut topology,
                &mut item_count,
                &reader,
                &collator,
            )?;
        } else {
            if !["electionDate", "electionName", "createdAt"].contains(&key.as_str()) {
                return Err(Error::Invalid("unknown feed header schema"));
            }
            header.insert(key, reader.value(limits.max_header)?);
            if crate::stringify(&Value::Object(header.clone()))?.len() > limits.max_header {
                return Err(Error::Invalid("feed header exceeds limit"));
            }
        }
        if reader.peek()? == Some(b'}') {
            break;
        }
        reader.token(b',')?;
        if reader.peek()? == Some(b'}') {
            return Err(Error::Invalid("trailing object comma"));
        }
    }
    reader.token(b'}')?;
    if reader.peek()?.is_some() {
        return Err(Error::Invalid("trailing JSON data"));
    }
    if !keys.contains("results")
        || !keys.contains("localResults")
        || header.get("electionDate").and_then(Value::as_str) != Some(&expectations.election_date)
        || header
            .get("electionName")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
    {
        return Err(Error::Invalid("invalid feed identity/date"));
    }
    let time = header
        .get("createdAt")
        .and_then(Value::as_str)
        .filter(|time| time.ends_with('Z'))
        .and_then(|time| DateTime::parse_from_rfc3339(time).ok())
        .ok_or(Error::Invalid("invalid source timestamp"))?;
    if time > Utc::now() + chrono::Duration::minutes(5) {
        return Err(Error::Invalid("source timestamp is in the future"));
    }
    topology.sort_unstable();
    let topology_sha256 = hex::encode(Sha256::digest(serde_json::to_vec(&topology)?));
    if !expectations.accepted_topologies.is_empty()
        && !expectations.accepted_topologies.contains(&topology_sha256)
    {
        return Err(Error::Invalid("source topology is not reviewed"));
    }
    if expectations
        .expected_jurisdictions
        .is_some_and(|count| count != scopes.len())
        || expectations.accepted_topologies.is_empty()
            && expectations
                .expected_items
                .is_some_and(|count| count != item_count)
    {
        return Err(Error::Invalid("source completeness mismatch"));
    }
    scopes.sort_by_key(|scope| scope.index);
    let spool_bytes = scopes.iter().try_fold(0_u64, |total, scope| {
        fs::metadata(&scope.path).map(|metadata| total.saturating_add(metadata.len()))
    })?;
    if spool_bytes > limits.max_download {
        return Err(Error::Invalid("scope spool exceeds disk limit"));
    }
    let jurisdictions = scopes.iter().map(|scope| scope.metadata.clone()).collect();
    hashes.sort_by(|a: &Value, b| {
        collator.compare(
            a["id"].as_str().unwrap_or_default(),
            b["id"].as_str().unwrap_or_default(),
        )
    });
    let mut meaningful_header = header.clone();
    meaningful_header.remove("createdAt");
    let fingerprint_value = json!({"header":meaningful_header,"scopes":hashes});
    let mut hash = HashWriter {
        hash: Sha256::new(),
        cancelled,
        deadline,
    };
    write_canonical(&fingerprint_value, "", &mut hash, &collator, 0)?;
    let fingerprint = hex::encode(hash.hash.finalize());
    cleanup.keep = true;
    Ok(Prepared {
        header,
        jurisdictions,
        scopes,
        fingerprint,
        topology_sha256,
        item_count,
    })
}

fn scope_membership(scope: &Value) -> Result<Vec<(String, Vec<String>)>> {
    let mut items: Vec<_> = scope["ballotItems"]
        .as_array()
        .ok_or(Error::Invalid("invalid ballot items"))?
        .iter()
        .map(|item| {
            let mut options: Vec<String> = item["ballotOptions"]
                .as_array()
                .ok_or(Error::Invalid("invalid ballot options"))?
                .iter()
                .map(|option| {
                    option["id"]
                        .as_str()
                        .map(str::to_owned)
                        .ok_or(Error::Invalid("invalid option ID"))
                })
                .collect::<Result<_>>()?;
            options.sort_unstable();
            Ok((
                item["id"]
                    .as_str()
                    .ok_or(Error::Invalid("invalid item ID"))?
                    .to_owned(),
                options,
            ))
        })
        .collect::<Result<_>>()?;
    items.sort_unstable();
    Ok(items)
}

type Topology = Vec<(String, String, Vec<(String, Vec<String>)>)>;

#[allow(clippy::too_many_arguments)]
fn process_scope(
    mut scope: Value,
    index: usize,
    directory: &Path,
    limits: &Limits,
    scopes: &mut Vec<Scope>,
    ids: &mut HashSet<String>,
    hashes: &mut Vec<Value>,
    topology: &mut Topology,
    item_count: &mut usize,
    reader: &Reader<'_>,
    collator: &icu_collator::CollatorBorrowed<'_>,
) -> Result<()> {
    reader.check()?;
    validate_scope(&scope, reader)?;
    let id = scope["id"]
        .as_str()
        .ok_or(Error::Invalid("invalid jurisdiction ID"))?
        .to_owned();
    if !ids.insert(id.clone()) {
        return Err(Error::Invalid("duplicate jurisdiction ID"));
    }
    topology.push((
        if index == 0 { "state" } else { "county" }.to_owned(),
        id.clone(),
        scope_membership(&scope)?,
    ));
    let mut hash = HashWriter {
        hash: Sha256::new(),
        cancelled: reader.cancelled,
        deadline: reader.deadline,
    };
    write_canonical(&scope, "", &mut hash, collator, 0)?;
    hashes.push(json!({"id":id,"hash":hex::encode(hash.hash.finalize())}));
    let path = directory.join(format!("scope-{index}.json"));
    let mut output = std::io::BufWriter::new(
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?,
    );
    // Source order and unknown fields survive; canonical bytes are only for hashes.
    let existing_spool_bytes = scopes.iter().try_fold(0_u64, |total, scope| {
        fs::metadata(&scope.path).map(|metadata| total.saturating_add(metadata.len()))
    })?;
    let mut checked = SpoolWriter {
        file: &mut output,
        bytes: 0,
        limit: (limits.max_scope as u64)
            .min(limits.max_download.saturating_sub(existing_spool_bytes)),
        cancelled: reader.cancelled,
        deadline: reader.deadline,
    };
    crate::canonical::write_json(&scope, &mut checked, 0)?;
    output.flush()?;
    let object = scope
        .as_object_mut()
        .ok_or(Error::Invalid("invalid jurisdiction"))?;
    let items = object
        .shift_remove("ballotItems")
        .ok_or(Error::Invalid("missing ballot items"))?;
    let items = items
        .as_array()
        .ok_or(Error::Invalid("invalid ballot items"))?;
    let mut batch_bytes = 2_usize;
    let mut batch_items = 0_usize;
    for (item_index, item) in items.iter().enumerate() {
        reader.check()?;
        let entry = json!({"jurisdiction_index":index,"item_index":item_index,"data":item});
        let bytes = crate::stringify(&entry)?.len();
        if bytes.saturating_add(2) > limits.max_batch_bytes {
            return Err(Error::Invalid("source item exceeds staging envelope"));
        }
        if batch_items >= limits.max_batch_items
            || batch_bytes
                .saturating_add(bytes)
                .saturating_add(usize::from(batch_items > 0))
                > limits.max_batch_bytes
        {
            batch_bytes = 2;
            batch_items = 0;
        }
        batch_bytes += bytes + usize::from(batch_items > 0);
        batch_items += 1;
    }
    *item_count = item_count
        .checked_add(items.len())
        .ok_or(Error::Invalid("source item count overflow"))?;
    // The reference bounds source metadata before adding the derived item count,
    // using JSON.stringify bytes rather than serde's scientific number notation.
    let mut metadata = Value::Object(std::mem::take(object));
    if crate::stringify(&metadata)?.len() > limits.max_metadata {
        return Err(Error::Invalid("jurisdiction metadata exceeds limit"));
    }
    metadata["item_count"] = json!(items.len());
    scopes.push(Scope {
        index,
        path,
        metadata,
    });
    Ok(())
}

fn label(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|text| !text.is_empty())
}
fn validate_scope(scope: &Value, reader: &Reader<'_>) -> Result<()> {
    if !scope.is_object()
        || !label(scope.get("id"))
        || !label(scope.get("name"))
        || !scope
            .get("reportingStatuses")
            .is_some_and(|value| value.is_null() || value.is_array())
    {
        return Err(Error::Invalid("invalid jurisdiction"));
    }
    let items = scope
        .get("ballotItems")
        .and_then(Value::as_array)
        .filter(|items| items.len() <= 99_999)
        .ok_or(Error::Invalid("invalid ballot items"))?;
    let mut ids = HashSet::new();
    for item in items {
        reader.check()?;
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or(Error::Invalid("invalid ballot item ID"))?;
        if !ids.insert(id) || !label(item.get("name")) {
            return Err(Error::Invalid("invalid or duplicate ballot item"));
        }
        let options = item
            .get("ballotOptions")
            .and_then(Value::as_array)
            .ok_or(Error::Invalid("invalid ballot choices"))?;
        let mut choices = HashSet::new();
        for option in options {
            let id = option
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or(Error::Invalid("invalid choice ID"))?;
            if !choices.insert(id) {
                return Err(Error::Invalid("duplicate choice ID"));
            }
            for field in ["groupResults", "precinctResults"] {
                if let Some(value) = option.get(field)
                    && !value.is_null()
                    && !value.is_array()
                {
                    return Err(Error::Invalid("invalid nested result array"));
                }
            }
        }
    }
    validate_counts(scope, 0, reader)
}
fn validate_counts(value: &Value, depth: usize, reader: &Reader<'_>) -> Result<()> {
    reader.check()?;
    if depth > 40 {
        return Err(Error::Invalid("source nesting exceeds limit"));
    }
    match value {
        Value::Array(values) => {
            for child in values {
                validate_counts(child, depth + 1, reader)?;
            }
        }
        Value::Object(values) => {
            for (key, child) in values {
                if ["precinctResults", "groupResults"].contains(&key.as_str())
                    && let Some(entries) = child.as_array()
                {
                    let identity = if key == "precinctResults" {
                        "id"
                    } else {
                        "groupName"
                    };
                    let mut ids = HashSet::new();
                    for entry in entries {
                        let id = entry
                            .get(identity)
                            .and_then(Value::as_str)
                            .ok_or(Error::Invalid("invalid nested result identity"))?;
                        if !ids.insert(id) {
                            return Err(Error::Invalid("duplicate nested result identity"));
                        }
                    }
                }
                if [
                    "voteCount",
                    "precinctsParticipating",
                    "precinctsReporting",
                    "registeredVoters",
                    "ballotsCast",
                    "voteFor",
                ]
                .contains(&key.as_str())
                    && !child.is_null()
                {
                    let number = child
                        .as_f64()
                        .ok_or(Error::Invalid("invalid source count"))?;
                    if number < 0.0 || number > 9_007_199_254_740_991.0 || number.fract() != 0.0 {
                        return Err(Error::Invalid("invalid source count"));
                    }
                }
                validate_counts(child, depth + 1, reader)?;
            }
        }
        _ => {}
    }
    Ok(())
}

struct Cleanup<'a> {
    path: &'a Path,
    keep: bool,
}
impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_dir_all(self.path);
        }
    }
}
struct SpoolWriter<'a> {
    file: &'a mut std::io::BufWriter<File>,
    bytes: u64,
    limit: u64,
    cancelled: &'a AtomicBool,
    deadline: Instant,
}
impl Write for SpoolWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.cancelled.load(Ordering::Relaxed) || Instant::now() >= self.deadline {
            return Err(std::io::Error::other("source serialization cancelled"));
        }
        if self.bytes.saturating_add(bytes.len() as u64) > self.limit {
            return Err(std::io::Error::other("scope spool exceeds limit"));
        }
        let written = self.file.write(bytes)?;
        self.bytes += written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}
struct HashWriter<'a> {
    hash: Sha256,
    cancelled: &'a AtomicBool,
    deadline: Instant,
}
impl Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.cancelled.load(Ordering::Relaxed) || Instant::now() >= self.deadline {
            return Err(std::io::Error::other("source hashing cancelled"));
        }
        self.hash.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Reader<'a> {
    buffer: BufReader<std::io::Take<File>>,
    read: u64,
    limits: &'a Limits,
    cancelled: &'a AtomicBool,
    deadline: Instant,
}
impl Reader<'_> {
    fn check(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Relaxed) || Instant::now() >= self.deadline {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
    fn peek_raw(&mut self) -> Result<Option<u8>> {
        self.check()?;
        Ok(self.buffer.fill_buf()?.first().copied())
    }
    fn next(&mut self) -> Result<Option<u8>> {
        let byte = self.peek_raw()?;
        if byte.is_some() {
            self.buffer.consume(1);
            self.read += 1;
            if self.read > self.limits.max_download {
                return Err(Error::Invalid("source download exceeds limit"));
            }
        }
        Ok(byte)
    }
    fn peek(&mut self) -> Result<Option<u8>> {
        while self
            .peek_raw()?
            .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.next()?;
        }
        self.peek_raw()
    }
    fn token(&mut self, expected: u8) -> Result<()> {
        if self.peek()? != Some(expected) {
            return Err(Error::Invalid("unexpected JSON token"));
        }
        self.next()?;
        Ok(())
    }
    fn value(&mut self, limit: usize) -> Result<Value> {
        let first = self.peek()?.ok_or(Error::Invalid("truncated JSON"))?;
        let structured = matches!(first, b'{' | b'[');
        let string = first == b'"';
        let mut bytes = Vec::new();
        let mut depth = 0_usize;
        let mut in_string = false;
        let mut escaped = false;
        loop {
            let Some(byte) = self.peek_raw()? else {
                if !structured && !in_string && !bytes.is_empty() {
                    break;
                }
                return Err(Error::Invalid("truncated JSON"));
            };
            if !structured
                && !string
                && !in_string
                && matches!(byte, b',' | b'}' | b']' | b' ' | b'\n' | b'\r' | b'\t')
            {
                break;
            }
            self.next()?;
            bytes.push(byte);
            if bytes.len() > limit {
                return Err(Error::Invalid("JSON value exceeds limit"));
            }
            if in_string {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    in_string = false;
                    if string && depth == 0 {
                        break;
                    }
                }
            } else if byte == b'"' {
                in_string = true;
            } else if matches!(byte, b'{' | b'[') {
                depth += 1;
                if depth > 40 {
                    return Err(Error::Invalid("source nesting exceeds limit"));
                }
            } else if matches!(byte, b'}' | b']') {
                depth = depth
                    .checked_sub(1)
                    .ok_or(Error::Invalid("invalid JSON nesting"))?;
                if structured && depth == 0 {
                    break;
                }
            }
        }
        let mut deserializer = serde_json::Deserializer::from_slice(&bytes);
        let value = UniqueValue::deserialize(&mut deserializer)?.0;
        deserializer.end()?;
        Ok(value)
    }
}

// serde_json::Value normally keeps the last duplicate object key. Reject instead.
struct UniqueValue(Value);
impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: de::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct UniqueVisitor;
        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = UniqueValue;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("unique-key JSON")
            }
            fn visit_bool<E: de::Error>(self, value: bool) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(json!(value)))
            }
            fn visit_i64<E: de::Error>(self, value: i64) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(json!(value)))
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(json!(value)))
            }
            fn visit_f64<E: de::Error>(self, value: f64) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(json!(value)))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(json!(value)))
            }
            fn visit_string<E: de::Error>(
                self,
                value: String,
            ) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(json!(value)))
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(UniqueValue(value)) = seq.next_element()? {
                    values.push(value);
                }
                Ok(UniqueValue(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = Map::new();
                while let Some((key, UniqueValue(value))) =
                    map.next_entry::<String, UniqueValue>()?
                {
                    if values.insert(key, value).is_some() {
                        return Err(de::Error::custom("duplicate JSON key"));
                    }
                }
                Ok(UniqueValue(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(UniqueVisitor)
    }
}
