//! Service-only bounded RPC transport. HTTP errors never include response bodies or URLs.
use crate::{Error, Result};
use reqwest::{
    Client, Url,
    header::{AUTHORIZATION, HeaderMap, HeaderValue},
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{sync::Arc, time::Duration};

#[derive(Clone)]
pub struct Rpc {
    client: Client,
    origin: Url,
    deadline: Duration,
    response_limit: usize,
    headers: Arc<HeaderMap>,
}
impl Rpc {
    /// Build a client for an HTTPS Supabase origin or explicit loopback test service.
    /// # Errors
    /// Rejects unsafe origins, credentials, and invalid headers.
    pub fn new(origin: &str, key: &str, deadline: Duration, response_limit: usize) -> Result<Self> {
        let mut origin = Url::parse(origin).map_err(|_| Error::Config("Invalid Supabase URL"))?;
        let local = matches!(origin.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if !(origin.scheme() == "https" || origin.scheme() == "http" && local)
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err(Error::Config(
                "Supabase URL must be a credential-free HTTPS origin",
            ));
        }
        if key.is_empty() || response_limit == 0 || deadline.is_zero() {
            return Err(Error::Config("RPC key and bounds are required"));
        }
        origin.set_path("/");
        let mut headers = HeaderMap::new();
        let mut api = HeaderValue::from_str(key).map_err(|_| Error::Config("Invalid RPC key"))?;
        api.set_sensitive(true);
        headers.insert("apikey", api);
        let mut authorization = HeaderValue::from_str(&format!("Bearer {key}"))
            .map_err(|_| Error::Config("Invalid RPC key"))?;
        authorization.set_sensitive(true);
        headers.insert(AUTHORIZATION, authorization);
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(deadline)
            .build()
            .map_err(|_| Error::Network)?;
        Ok(Self {
            client,
            origin,
            deadline,
            response_limit,
            headers: Arc::new(headers),
        })
    }
    /// Invoke a fixed service RPC with a bounded body and total deadline.
    /// # Errors
    /// Returns sanitized transport, HTTP, JSON, or response-size failures.
    pub async fn call<T: DeserializeOwned>(&self, name: &'static str, args: Value) -> Result<T> {
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
            return Err(Error::Config("Invalid RPC name"));
        }
        let url = self
            .origin
            .join(&format!("rest/v1/rpc/{name}"))
            .map_err(|_| Error::Config("Invalid RPC URL"))?;
        let request = async {
            let mut response = self
                .client
                .post(url)
                .headers((*self.headers).clone())
                .json(&args)
                .send()
                .await
                .map_err(|error| transport(&error))?;
            if !response.status().is_success() {
                return Err(Error::Http(response.status().as_u16()));
            }
            if response
                .content_length()
                .is_some_and(|n| n > self.response_limit as u64)
            {
                return Err(Error::ResponseLimit);
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|error| transport(&error))? {
                if chunk.len() > self.response_limit.saturating_sub(bytes.len()) {
                    return Err(Error::ResponseLimit);
                }
                bytes.extend_from_slice(&chunk);
            }
            if bytes.is_empty() {
                bytes.extend_from_slice(b"null");
            }
            serde_json::from_slice(&bytes).map_err(|_| Error::InvalidResponse)
        };
        tokio::time::timeout(self.deadline, request)
            .await
            .map_err(|_| Error::Timeout)?
    }
}
fn transport(error: &reqwest::Error) -> Error {
    if error.is_timeout() {
        Error::Timeout
    } else {
        Error::Network
    }
}
