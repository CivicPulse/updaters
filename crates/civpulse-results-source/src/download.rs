use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, SystemTime},
};

use chrono::{DateTime, Utc};
use reqwest::{Client, header};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::{Error, Limits, Result};

/// Validators from the durable job's preceding source receipt.
#[derive(Clone, Debug, Default)]
pub struct Validators {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// Exact downloaded bytes plus server validators and retrieval time.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub sha256: String,
    pub bytes: u64,
    pub retrieved_at: DateTime<Utc>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// A conditional source check is successful even when no new body exists.
#[derive(Clone, Debug)]
pub enum Download {
    Unchanged {
        etag: Option<String>,
        last_modified: Option<String>,
        retrieved_at: DateTime<Utc>,
    },
    Downloaded(Receipt),
}

/// Parse numeric or HTTP-date Retry-After, bounded to 1..=3600 seconds.
#[must_use]
pub fn retry_after(value: Option<&str>, now: SystemTime) -> u64 {
    value
        .and_then(|value| {
            value.parse::<u64>().ok().or_else(|| {
                httpdate::parse_http_date(value).ok().map(|date| {
                    date.duration_since(now).map_or(1, |duration| {
                        duration
                            .as_secs()
                            .saturating_add(u64::from(duration.subsec_nanos() != 0))
                    })
                })
            })
        })
        .unwrap_or(60)
        .clamp(1, 3600)
}

/// Download only the exact reviewed official HTTPS URL into a new spool file.
///
/// The request forbids redirects and compression, streams exact bytes/SHA-256,
/// enforces one total deadline and removes partial files on any failure. The
/// caller owns successful files and must delete them after durable archiving.
/// Retries belong to the fenced durable job (this function returns Retry-After).
///
/// # Errors
/// Rejects unapproved URLs, size/deadline violations, HTTP errors and truncation.
pub async fn download(
    url: &str,
    allowed_url: &str,
    path: &Path,
    validators: &Validators,
    limits: &Limits,
    cancelled: &AtomicBool,
) -> Result<Download> {
    let parsed = reqwest::Url::parse(url).map_err(|_| Error::Invalid("invalid source URL"))?;
    let name = parsed
        .path()
        .strip_prefix("/cdn/results/Georgia/export-")
        .and_then(|name| name.strip_suffix(".json"));
    if url != allowed_url
        || parsed.scheme() != "https"
        || parsed.host_str() != Some("results.sos.ga.gov")
        || parsed.port().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !name.is_some_and(|name| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
    {
        return Err(Error::Invalid(
            "source URL is not the exact approved SOS export",
        ));
    }
    transfer(url, path, validators, limits, cancelled).await
}

async fn transfer(
    url: &str,
    path: &Path,
    validators: &Validators,
    limits: &Limits,
    cancelled: &AtomicBool,
) -> Result<Download> {
    let operation = async {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(30))
            .timeout(limits.download_timeout)
            .build()
            .map_err(Error::Http)?;
        let mut request = client
            .get(url)
            .header(header::ACCEPT, "application/json")
            .header(header::ACCEPT_ENCODING, "identity");
        if let Some(etag) = &validators.etag {
            request = request.header(header::IF_NONE_MATCH, etag);
        } else if let Some(modified) = &validators.last_modified {
            request = request.header(header::IF_MODIFIED_SINCE, modified);
        }
        let mut response = request.send().await.map_err(Error::Http)?;
        let retrieved_at = Utc::now();
        let etag = response
            .headers()
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let last_modified = response
            .headers()
            .get(header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if response.status() == 304 {
            return Ok(Download::Unchanged {
                etag,
                last_modified,
                retrieved_at,
            });
        }
        if !response.status().is_success() {
            return Err(Error::Status {
                status: response.status().as_u16(),
                retry_after: retry_after(
                    response
                        .headers()
                        .get(header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok()),
                    SystemTime::now(),
                ),
            });
        }
        if response
            .headers()
            .get(header::CONTENT_ENCODING)
            .is_some_and(|value| value != "identity")
        {
            return Err(Error::Invalid("encoded source response is unsupported"));
        }
        let expected_length = response.content_length();
        if expected_length.is_some_and(|length| length > limits.max_download) {
            return Err(Error::Invalid("source download exceeds limit"));
        }
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        let mut file = tokio::fs::File::from_std(file);
        let mut partial = PartialFile { path, keep: false };
        let mut hash = Sha256::new();
        let mut bytes = 0_u64;
        while let Some(chunk) = response.chunk().await.map_err(Error::Http)? {
            bytes = bytes
                .checked_add(chunk.len() as u64)
                .ok_or(Error::Invalid("source size overflow"))?;
            if bytes > limits.max_download {
                return Err(Error::Invalid("source download exceeds limit"));
            }
            hash.update(&chunk);
            file.write_all(&chunk).await?;
        }
        if bytes == 0 || expected_length.is_some_and(|length| length != bytes) {
            return Err(Error::Invalid("empty or truncated source response"));
        }
        file.flush().await?;
        drop(file);
        partial.keep = true;
        Ok(Download::Downloaded(Receipt {
            sha256: format!("{:x}", hash.finalize()),
            bytes,
            retrieved_at,
            etag,
            last_modified,
        }))
    };
    tokio::select! {
        result = tokio::time::timeout(limits.download_timeout, operation) => result.map_err(|_| Error::Cancelled)?,
        () = cancellation(cancelled) => Err(Error::Cancelled),
    }
}

async fn cancellation(cancelled: &AtomicBool) {
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

struct PartialFile<'a> {
    path: &'a Path,
    keep: bool,
}
impl Drop for PartialFile<'_> {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::mpsc,
    };

    fn server(response: &'static [u8]) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while socket.read(&mut byte).unwrap_or(0) > 0 {
                request.push(byte[0]);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let _ = send.send(String::from_utf8(request).unwrap());
            let _ = socket.write_all(response);
        });
        (format!("http://{address}/source"), receive)
    }

    #[tokio::test]
    async fn conditional_get_archives_exact_bom_whitespace_and_checksum() {
        let (url,request)=server(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nETag: \"new\"\r\nConnection: close\r\n\r\n\xef\xbb\xbf {}  \n ");
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("raw");
        let validators = Validators {
            etag: Some("\"old\"".to_owned()),
            last_modified: Some("ignored".to_owned()),
        };
        let result = transfer(
            &url,
            &path,
            &validators,
            &Limits::default(),
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        let Download::Downloaded(receipt) = result else {
            panic!("download expected")
        };
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes, b"\xef\xbb\xbf {}  \n ");
        assert_eq!(receipt.sha256, format!("{:x}", Sha256::digest(&bytes)));
        assert_eq!(receipt.bytes, 10);
        assert_eq!(receipt.etag.as_deref(), Some("\"new\""));
        let request = request.recv().unwrap().to_lowercase();
        assert!(request.contains("if-none-match: \"old\""));
        assert!(!request.contains("if-modified-since"));
        assert!(request.contains("accept-encoding: identity"));
    }

    #[tokio::test]
    async fn unchanged_and_last_modified_checks_create_no_file() {
        let (url, request) =
            server(b"HTTP/1.1 304 Not Modified\r\nETag: \"same\"\r\nConnection: close\r\n\r\n");
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("raw");
        assert!(matches!(
            transfer(
                &url,
                &path,
                &Validators {
                    etag: None,
                    last_modified: Some("Tue, 03 Nov 2026 10:00:00 GMT".to_owned())
                },
                &Limits::default(),
                &AtomicBool::new(false)
            )
            .await
            .unwrap(),
            Download::Unchanged { .. }
        ));
        assert!(!path.exists());
        assert!(
            request
                .recv()
                .unwrap()
                .to_lowercase()
                .contains("if-modified-since:")
        );
    }

    #[tokio::test]
    async fn truncation_oversize_redirects_rate_limits_and_outages_fail_closed() {
        for response in [
            &b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\nConnection: close\r\n\r\n{}"[..],
            &b"HTTP/1.1 200 OK\r\nContent-Length: 999\r\nConnection: close\r\n\r\n{}"[..],
            &b"HTTP/1.1 302 Found\r\nLocation: https://example.com/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"[..],
            &b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 123\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"[..],
            &b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"[..],
            &b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Encoding: gzip\r\nConnection: close\r\n\r\n{}"[..],
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\n12345\r\n0\r\n\r\n"[..],
        ] {
            let (url,_)=server(response);let temp=tempfile::tempdir().unwrap();let path=temp.path().join("raw");
            let result=transfer(&url,&path,&Validators::default(),&Limits {max_download:4,..Limits::default()},&AtomicBool::new(false)).await;
            assert!(result.is_err());assert!(!path.exists());
            if response.windows(3).any(|bytes|bytes==b"429") {assert!(matches!(result,Err(Error::Status {status:429,retry_after:123})));}
        }
    }

    #[tokio::test]
    async fn exact_url_and_cancellation_are_checked_without_downloading() {
        let good = "https://results.sos.ga.gov/cdn/results/Georgia/export-11032026GeneralAndSpecialElection.json";
        let temp = tempfile::tempdir().unwrap();
        for bad in [
            "http://results.sos.ga.gov/cdn/results/Georgia/export-a.json",
            "https://results.sos.ga.gov.evil.test/cdn/results/Georgia/export-a.json",
            "https://results.sos.ga.gov/cdn/results/Georgia/export-a.json?secret=x",
            "https://results.sos.ga.gov/cdn/results/Georgia/export-a.json",
        ] {
            assert!(
                download(
                    bad,
                    good,
                    &temp.path().join("raw"),
                    &Validators::default(),
                    &Limits::default(),
                    &AtomicBool::new(false)
                )
                .await
                .is_err()
            );
        }
        assert!(
            download(
                good,
                good,
                &temp.path().join("raw"),
                &Validators::default(),
                &Limits::default(),
                &AtomicBool::new(true)
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn missing_validators_length_and_stalled_body_are_handled() {
        let (url, request) = server(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{}");
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("raw");
        assert!(matches!(
            transfer(
                &url,
                &path,
                &Validators::default(),
                &Limits::default(),
                &AtomicBool::new(false)
            )
            .await
            .unwrap(),
            Download::Downloaded(Receipt { bytes: 2, .. })
        ));
        let request = request.recv().unwrap().to_lowercase();
        assert!(!request.contains("if-none-match") && !request.contains("if-modified-since"));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = [0; 4096];
            let _ = socket.read(&mut request);
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{");
            std::thread::sleep(Duration::from_millis(100));
        });
        let path = temp.path().join("stalled");
        assert!(
            transfer(
                &url,
                &path,
                &Validators::default(),
                &Limits {
                    download_timeout: Duration::from_millis(25),
                    ..Limits::default()
                },
                &AtomicBool::new(false)
            )
            .await
            .is_err()
        );
        assert!(!path.exists());
        handle.join().unwrap();
    }

    #[test]
    fn retry_after_numeric_dates_and_bad_values_are_bounded() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        for (value, expected) in [
            (None, 60),
            (Some("0"), 1),
            (Some("999999"), 3600),
            (Some("invalid"), 60),
            (Some("12"), 12),
            (Some("Thu, 01 Jan 1970 00:17:00 GMT"), 20),
        ] {
            assert_eq!(retry_after(value, now), expected);
        }
    }
}
