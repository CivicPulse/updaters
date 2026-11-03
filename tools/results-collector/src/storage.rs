//! Bounded S3 transport and atomic publication. Only the consistent S3 API is read.
use crate::{Error, Result};
use async_trait::async_trait;
use aws_credential_types::Credentials;
use aws_sigv4::{
    http_request::{PayloadChecksumKind, SignableBody, SignableRequest, SigningSettings, sign},
    sign::v4,
};
use reqwest::{Client, Method, Url};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    time::{Duration, SystemTime},
};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

pub const IMMUTABLE_CACHE: &str = "public, max-age=31536000, immutable";
pub const POINTER_CACHE: &str = "public, max-age=15, s-maxage=15, must-revalidate";
pub const MAX_OBJECT: u64 = 512 * 1024 * 1024;
pub const MAX_POINTER: usize = 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(180);

#[derive(Clone)]
pub struct Stored {
    pub status: u16,
    pub etag: Option<String>,
    pub body: Vec<u8>,
}
#[derive(Clone, Default)]
pub struct Conditions {
    pub if_match: Option<String>,
    pub if_none_match: bool,
    pub cache_control: &'static str,
}
#[async_trait]
pub trait Store: Send + Sync + 'static {
    /// Resolve a validated immutable/discovery key on the configured public origin.
    /// # Errors
    /// Rejects invalid object keys.
    fn url(&self, key: &str) -> Result<String>;
    async fn get(&self, key: &str, limit: usize) -> Result<Stored>;
    async fn put(&self, key: &str, bytes: &[u8], conditions: Conditions) -> Result<u16>;
    async fn put_file(
        &self,
        key: &str,
        path: &Path,
        hash: &str,
        bytes: u64,
        conditions: Conditions,
    ) -> Result<u16>;
    /// Stream an object into a local file, verify exact hash/size, never use CDN bytes.
    async fn download(&self, key: &str, path: &Path, hash: &str, bytes: u64) -> Result<()>;
    async fn verify(&self, key: &str, hash: &str, bytes: u64) -> Result<()>;
}
#[derive(Clone)]
pub struct R2Store {
    client: Client,
    endpoint: Url,
    bucket: String,
    public_origin: String,
    credentials: Credentials,
}
impl R2Store {
    /// Only the environment's exact bucket and public origin pair may be selected.
    /// # Errors
    /// Rejects invalid environment, endpoint, credentials, bucket, or public origin.
    pub fn new(
        environment: &str,
        endpoint: &str,
        bucket: &str,
        origin: &str,
        access: &str,
        secret: &str,
    ) -> Result<Self> {
        let expected = match environment {
            "prod" => ("civpulse-files", "https://files.civpulse.org"),
            "dev" => ("civpulse-files-dev", "https://files-dev.civpulse.org"),
            "test" => ("civpulse-files-test", "https://files-test.civpulse.org"),
            _ => return Err(Error::Config("Environment must be dev, test or prod")),
        };
        if (bucket, origin.trim_end_matches('/')) != expected
            || access.is_empty()
            || secret.is_empty()
        {
            return Err(Error::Config(
                "R2 environment bucket/origin mismatch or missing credentials",
            ));
        }
        let endpoint = Url::parse(endpoint).map_err(|_| Error::Config("Invalid R2 endpoint"))?;
        let host = endpoint.host_str().unwrap_or_default();
        let account = host
            .strip_suffix(".r2.cloudflarestorage.com")
            .unwrap_or_default();
        if endpoint.scheme() != "https"
            || account.len() != 32
            || !account
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || endpoint.path() != "/"
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.port().is_some()
        {
            return Err(Error::Config("R2 requires an account HTTPS endpoint"));
        }
        Ok(Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(DEADLINE)
                .build()
                .map_err(|_| Error::Storage("Cannot build R2 transport"))?,
            endpoint,
            bucket: bucket.into(),
            public_origin: expected.1.into(),
            credentials: Credentials::new(access, secret, None, None, "civpulse-r2"),
        })
    }
    fn signed(
        &self,
        method: Method,
        key: &str,
        hash: String,
        bytes: Option<u64>,
        conditions: &Conditions,
    ) -> Result<reqwest::RequestBuilder> {
        validate_key(key)?;
        let url = self
            .endpoint
            .join(&format!("{}/{key}", self.bucket))
            .map_err(|_| Error::Storage("Invalid object URL"))?;
        let mut headers = vec![("x-amz-content-sha256".to_owned(), hash.clone())];
        if method == Method::PUT {
            headers.push((
                "content-type".into(),
                "application/json; charset=utf-8".into(),
            ));
            headers.push(("cache-control".into(), conditions.cache_control.into()));
            if conditions.if_none_match {
                headers.push(("if-none-match".into(), "*".into()));
            }
            if let Some(etag) = &conditions.if_match {
                headers.push(("if-match".into(), etag.clone()));
            }
            if let Some(size) = bytes {
                headers.push(("content-length".into(), size.to_string()));
            }
        }
        let identity = self.credentials.clone().into();
        let mut settings = SigningSettings::default();
        settings.payload_checksum_kind = PayloadChecksumKind::XAmzSha256;
        let params = v4::SigningParams::builder()
            .identity(&identity)
            .region("auto")
            .name("s3")
            .time(SystemTime::now())
            .settings(settings)
            .build()
            .map_err(|_| Error::Storage("R2 signing configuration"))?
            .into();
        let request = SignableRequest::new(
            method.as_str(),
            url.as_str(),
            headers.iter().map(|(k, v)| (k.as_str(), v.as_str())),
            SignableBody::Precomputed(hash),
        )
        .map_err(|_| Error::Storage("R2 signable request"))?;
        let (instructions, _) = sign(request, &params)
            .map_err(|_| Error::Storage("R2 signature failed"))?
            .into_parts();
        let mut signed_headers = reqwest::header::HeaderMap::new();
        for (key, value) in headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .chain(instructions.headers())
        {
            let name = reqwest::header::HeaderName::from_bytes(key.as_bytes())
                .map_err(|_| Error::Storage("Invalid signed header"))?;
            let mut value = reqwest::header::HeaderValue::from_str(value)
                .map_err(|_| Error::Storage("Invalid signed header"))?;
            if name == reqwest::header::AUTHORIZATION {
                value.set_sensitive(true);
            }
            signed_headers.insert(name, value);
        }
        Ok(self.client.request(method, url).headers(signed_headers))
    }
    async fn response(&self, key: &str) -> Result<reqwest::Response> {
        self.signed(Method::GET, key, sha256(&[]), None, &Conditions::default())?
            .send()
            .await
            .map_err(|_| Error::Storage("R2 GET transport failed"))
    }
    async fn stream(
        &self,
        key: &str,
        path: Option<&Path>,
        expected_hash: &str,
        expected_bytes: u64,
    ) -> Result<()> {
        if expected_bytes > MAX_OBJECT || expected_bytes == 0 || !valid_hash(expected_hash) {
            return Err(Error::Storage("Invalid object receipt"));
        }
        let operation = async {
            let mut response = self.response(key).await?;
            if response.status().as_u16() != 200
                || response
                    .content_length()
                    .is_some_and(|n| n != expected_bytes)
            {
                return Err(Error::Storage("R2 readback status/size mismatch"));
            }
            let mut output = match path {
                Some(path) => Some(
                    tokio::fs::File::create(path)
                        .await
                        .map_err(|_| Error::Storage("Cannot create archived source spool"))?,
                ),
                None => None,
            };
            let mut hasher = Sha256::new();
            let mut size = 0_u64;
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| Error::Storage("R2 readback stream failed"))?
            {
                size = size
                    .checked_add(chunk.len() as u64)
                    .ok_or(Error::Storage("R2 readback exceeds receipt"))?;
                if size > expected_bytes {
                    return Err(Error::Storage("R2 readback exceeds receipt"));
                }
                hasher.update(&chunk);
                if let Some(file) = &mut output {
                    file.write_all(&chunk)
                        .await
                        .map_err(|_| Error::Storage("Cannot spool archived source"))?;
                }
            }
            if size != expected_bytes || format!("{:x}", hasher.finalize()) != expected_hash {
                return Err(Error::Storage("R2 immutable checksum mismatch"));
            }
            if let Some(file) = &mut output {
                file.flush()
                    .await
                    .map_err(|_| Error::Storage("Cannot flush archive spool"))?;
            }
            Ok(())
        };
        tokio::time::timeout(DEADLINE, operation)
            .await
            .map_err(|_| Error::Storage("R2 readback deadline exceeded"))?
    }
}
#[async_trait]
impl Store for R2Store {
    fn url(&self, key: &str) -> Result<String> {
        validate_key(key)?;
        Ok(format!("{}/{key}", self.public_origin))
    }
    async fn get(&self, key: &str, limit: usize) -> Result<Stored> {
        let operation = async {
            let mut response = self.response(key).await?;
            let status = response.status().as_u16();
            let etag = response
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            if status != 200 {
                return Ok(Stored {
                    status,
                    etag,
                    body: vec![],
                });
            }
            if response.content_length().is_some_and(|n| n > limit as u64) {
                return Err(Error::Storage("R2 JSON exceeds limit"));
            }
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| Error::Storage("R2 JSON read failed"))?
            {
                if chunk.len() > limit.saturating_sub(body.len()) {
                    return Err(Error::Storage("R2 JSON exceeds limit"));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(Stored { status, etag, body })
        };
        tokio::time::timeout(DEADLINE, operation)
            .await
            .map_err(|_| Error::Storage("R2 JSON deadline exceeded"))?
    }
    async fn put(&self, key: &str, bytes: &[u8], conditions: Conditions) -> Result<u16> {
        if bytes.len() as u64 > MAX_OBJECT {
            return Err(Error::Storage("R2 upload exceeds limit"));
        }
        Ok(self
            .signed(
                Method::PUT,
                key,
                sha256(bytes),
                Some(bytes.len() as u64),
                &conditions,
            )?
            .body(bytes.to_owned())
            .send()
            .await
            .map_err(|_| Error::Storage("R2 PUT failed"))?
            .status()
            .as_u16())
    }
    async fn put_file(
        &self,
        key: &str,
        path: &Path,
        hash: &str,
        bytes: u64,
        conditions: Conditions,
    ) -> Result<u16> {
        if bytes > MAX_OBJECT || !valid_hash(hash) {
            return Err(Error::Storage("Invalid file upload receipt"));
        }
        let file = tokio::fs::File::open(path)
            .await
            .map_err(|_| Error::Storage("Cannot open raw spool"))?;
        if file
            .metadata()
            .await
            .map_err(|_| Error::Storage("Cannot read raw spool size"))?
            .len()
            != bytes
        {
            return Err(Error::Storage("Raw spool size changed"));
        }
        Ok(self
            .signed(Method::PUT, key, hash.into(), Some(bytes), &conditions)?
            .body(reqwest::Body::wrap_stream(ReaderStream::new(file)))
            .send()
            .await
            .map_err(|_| Error::Storage("R2 file PUT failed"))?
            .status()
            .as_u16())
    }
    async fn download(&self, key: &str, path: &Path, hash: &str, bytes: u64) -> Result<()> {
        self.stream(key, Some(path), hash, bytes).await
    }
    async fn verify(&self, key: &str, hash: &str, bytes: u64) -> Result<()> {
        self.stream(key, None, hash, bytes).await
    }
}
/// Reject traversal, unreviewed roots and unsafe source-derived path components.
/// # Errors
/// Rejects invalid UUIDs and any non-ASCII safe object-key component.
pub fn validate_key(key: &str) -> Result<()> {
    let mut parts = key.split('/');
    if parts.next() != Some("elections")
        || parts
            .next()
            .and_then(|v| uuid::Uuid::parse_str(v).ok())
            .is_none()
    {
        return Err(Error::Storage("Invalid results object key"));
    }
    let rest: Vec<_> = parts.collect();
    if rest.is_empty()
        || !matches!(rest[0], "raw" | "revisions" | "latest.json")
        || rest.iter().any(|part| {
            part.is_empty()
                || part.contains("..")
                || !part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        })
    {
        return Err(Error::Storage("Invalid results object key"));
    }
    Ok(())
}
#[must_use]
pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
/// Create immutable bytes and verify exact content even on a create-only collision.
/// # Errors
/// Rejects failed uploads, missing objects, or checksum/size mismatches.
pub async fn immutable(store: &dyn Store, key: &str, bytes: &[u8]) -> Result<()> {
    let hash = sha256(bytes);
    let status = store
        .put(
            key,
            bytes,
            Conditions {
                if_none_match: true,
                cache_control: IMMUTABLE_CACHE,
                ..Conditions::default()
            },
        )
        .await?;
    if !(200..300).contains(&status) && status != 412 {
        return Err(Error::Storage("Immutable PUT rejected"));
    }
    store.verify(key, &hash, bytes.len() as u64).await
}
/// Create raw evidence without buffering the whole statewide source.
/// # Errors
/// Rejects failed uploads or immutable checksum collisions.
pub async fn immutable_file(
    store: &dyn Store,
    key: &str,
    path: &Path,
    hash: &str,
    bytes: u64,
) -> Result<()> {
    let status = store
        .put_file(
            key,
            path,
            hash,
            bytes,
            Conditions {
                if_none_match: true,
                cache_control: IMMUTABLE_CACHE,
                ..Conditions::default()
            },
        )
        .await?;
    if !(200..300).contains(&status) && status != 412 {
        return Err(Error::Storage("Raw immutable PUT rejected"));
    }
    store.verify(key, hash, bytes).await
}
/// Atomically advance the stable pointer. A delayed worker can never regress it.
/// # Errors
/// Rejects conflicting equal revisions, invalid existing pointers, or exhausted CAS retries.
pub async fn publish_pointer<F, Fut>(
    store: &dyn Store,
    pointer: &Value,
    mut confirm: F,
) -> Result<&'static str>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<bool>>,
{
    let (election, snapshot, revision) = pointer_identity(pointer)?;
    let key = format!("elections/{election}/latest.json");
    let bytes =
        serde_json::to_vec(pointer).map_err(|_| Error::Storage("Pointer serialization failed"))?;
    if bytes.len() > MAX_POINTER {
        return Err(Error::Storage("Discovery pointer exceeds limit"));
    }
    for _ in 0..4 {
        let prior = store.get(&key, MAX_POINTER).await?;
        let condition = if prior.status == 200 {
            let value: Value = serde_json::from_slice(&prior.body)
                .map_err(|_| Error::Storage("Invalid existing pointer"))?;
            let (prior_election, prior_snapshot, prior_revision) = pointer_identity(&value)?;
            if prior_election != election {
                return Err(Error::Storage("Pointer election mismatch"));
            }
            if prior_revision > revision {
                return Ok("superseded");
            }
            if prior_revision == revision {
                if prior_snapshot != snapshot {
                    return Err(Error::Storage("Conflicting pointer revision"));
                }
                return Ok("current");
            }
            Conditions {
                if_match: Some(prior.etag.ok_or(Error::Storage("Pointer ETag missing"))?),
                cache_control: POINTER_CACHE,
                ..Conditions::default()
            }
        } else if prior.status == 404 {
            Conditions {
                if_none_match: true,
                cache_control: POINTER_CACHE,
                ..Conditions::default()
            }
        } else {
            return Err(Error::Storage("Pointer read failed"));
        };
        if !confirm().await? {
            return Ok("superseded");
        }
        let written = store.put(&key, &bytes, condition).await?;
        if written == 412 {
            continue;
        }
        if !(200..300).contains(&written) {
            return Err(Error::Storage("Pointer PUT failed"));
        }
        let readback = store.get(&key, MAX_POINTER).await?;
        if readback.status != 200 {
            return Err(Error::Storage("Pointer readback failed"));
        }
        let actual: Value = serde_json::from_slice(&readback.body)
            .map_err(|_| Error::Storage("Invalid pointer readback"))?;
        let (actual_election, actual_snapshot, actual_revision) = pointer_identity(&actual)?;
        if actual_election != election
            || actual_revision < revision
            || actual_revision == revision && actual_snapshot != snapshot
        {
            return Err(Error::Storage("Pointer readback identity mismatch"));
        }
        return Ok(if actual_revision == revision {
            "published"
        } else {
            "superseded"
        });
    }
    Err(Error::Storage(
        "Pointer CAS contention; durable retry required",
    ))
}
fn pointer_identity(pointer: &Value) -> Result<(&str, &str, civpulse_results_contract::Revision)> {
    let election = pointer["election_id"]
        .as_str()
        .filter(|v| uuid::Uuid::parse_str(v).is_ok())
        .ok_or(Error::Storage("Invalid pointer election"))?;
    let snapshot = pointer["snapshot_id"]
        .as_str()
        .filter(|v| uuid::Uuid::parse_str(v).is_ok())
        .ok_or(Error::Storage("Invalid pointer snapshot"))?;
    if pointer["schema_version"] != 1 {
        return Err(Error::Storage("Invalid pointer schema"));
    }
    let revision = serde_json::from_value(pointer["revision"].clone())
        .map_err(|_| Error::Storage("Invalid pointer revision"))?;
    Ok((election, snapshot, revision))
}
