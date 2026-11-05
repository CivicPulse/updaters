use crate::{backend::Backend, catalog, handlers};
use anyhow::{Context, Result, bail};
use axum::{
    Router,
    body::to_bytes,
    extract::{Request, State},
    http::{HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use url::Url;

#[derive(Clone)]
pub struct Config {
    pub listen: String,
    pub supabase: Url,
    pub public_key: String,
    /// Canonical V2 resource URL, including `/v2`.
    pub resource: Url,
    pub issuer: String,
    pub kb: Option<Url>,
    pub georgia: Option<Url>,
}
impl Config {
    /// Load service configuration.
    ///
    /// # Errors
    /// Returns an error for missing or invalid environment variables.
    pub fn from_env() -> Result<Self> {
        let required = |key| std::env::var(key).with_context(|| format!("Missing {key}"));
        let optional = |key| -> Result<Option<Url>> {
            std::env::var(key)
                .ok()
                .map(|s| Url::parse(&s))
                .transpose()
                .map_err(Into::into)
        };
        let config = Self {
            listen: std::env::var("MCP_LISTEN").unwrap_or_else(|_| "0.0.0.0:8789".into()),
            supabase: Url::parse(&required("SUPABASE_URL")?)?,
            public_key: required("SUPABASE_ANON_KEY")?,
            resource: Url::parse(&required("MCP_RESOURCE_URL")?)?,
            issuer: required("AUTHORIZATION_SERVER")?,
            kb: optional("KB_API_URL")?,
            georgia: optional("GEORGIA_SOURCE_READER_URL")?,
        };
        config.validate()?;
        Ok(config)
    }
    fn validate(&self) -> Result<()> {
        for url in std::iter::once(&self.supabase)
            .chain(std::iter::once(&self.resource))
            .chain(self.kb.iter())
            .chain(self.georgia.iter())
        {
            if (url.scheme() != "https"
                && !(url.scheme() == "http"
                    && matches!(url.host_str(), Some("localhost" | "127.0.0.1"))))
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                bail!(
                    "Use HTTPS service URLs without credentials, query or fragment (HTTP loopback allowed)"
                );
            }
        }
        if self.public_key.is_empty() {
            bail!("SUPABASE_ANON_KEY is empty");
        }
        if self.public_key.starts_with("sb_secret_") {
            bail!("Use a public Supabase key, never a secret key");
        }
        if let Some(payload) = self
            .public_key
            .split('.')
            .nth(1)
            .and_then(|s| URL_SAFE_NO_PAD.decode(s).ok())
            .and_then(|v| serde_json::from_slice::<Value>(&v).ok())
        {
            if payload["role"] == "service_role" {
                bail!("Use a public Supabase key, never service_role");
            }
        }
        Url::parse(&self.issuer).context("Invalid AUTHORIZATION_SERVER")?;
        Ok(())
    }
}
struct App {
    config: Config,
    client: reqwest::Client,
}
/// Build the HTTP MCP service.
///
/// # Errors
/// Returns an error for invalid URLs, keys or HTTP client configuration.
pub fn router(config: Config) -> Result<Router> {
    config.validate()?;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    Ok(Router::new()
        .fallback(handle)
        .with_state(Arc::new(App { config, client })))
}
fn response(status: StatusCode, value: Value) -> Response {
    (status, axum::Json(value)).into_response()
}
fn allowed_origin(origin: &str) -> bool {
    matches!(
        origin,
        "https://www.civpulse.org"
            | "https://new.civpulse.org"
            | "https://civpulse.org"
            | "https://api.civpulse.org"
            | "http://localhost:8789"
            | "http://127.0.0.1:8789"
    )
}
async fn handle(State(app): State<Arc<App>>, request: Request) -> Response {
    let origin = request
        .headers()
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let mut result = respond(&app, request).await;
    result
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    if result.status() != StatusCode::FORBIDDEN {
        if let Some(origin) = origin.filter(|s| allowed_origin(s)) {
            let headers = result.headers_mut();
            headers.insert(
                "access-control-allow-origin",
                HeaderValue::from_str(&origin).expect("checked literal"),
            );
            headers.insert(
                "access-control-expose-headers",
                HeaderValue::from_static("WWW-Authenticate"),
            );
            headers.insert("vary", HeaderValue::from_static("Origin"));
        }
    }
    result
}
fn normalized(path: &str) -> &str {
    if path.starts_with("/functions/v1/") {
        &path[13..]
    } else {
        path
    }
}
#[allow(clippy::too_many_lines)] // Keep the ordered HTTP security and protocol gates together.
async fn respond(app: &App, request: Request) -> Response {
    let c = &app.config;
    let host = request.uri().host().or_else(|| {
        request
            .headers()
            .get("host")
            .and_then(|s| s.to_str().ok())
            .and_then(|s| s.split(':').next())
    });
    if !host.is_some_and(|h| {
        Some(h) == c.resource.host_str()
            || Some(h) == c.supabase.host_str()
            || h == "localhost"
            || h == "127.0.0.1"
    }) {
        return (StatusCode::FORBIDDEN, "Forbidden host").into_response();
    }
    if request
        .headers()
        .get("origin")
        .is_some_and(|s| s.to_str().map_or(true, |s| !allowed_origin(s)))
    {
        return (StatusCode::FORBIDDEN, "Forbidden origin").into_response();
    }
    if request.method() == Method::OPTIONS {
        let mut r = StatusCode::NO_CONTENT.into_response();
        r.headers_mut().insert(
            "access-control-allow-methods",
            HeaderValue::from_static("GET, POST, DELETE, OPTIONS"),
        );
        r.headers_mut().insert(
            "access-control-allow-headers",
            HeaderValue::from_static(
                "Authorization, Content-Type, Accept, MCP-Protocol-Version, MCP-Session-Id",
            ),
        );
        return r;
    }
    let path = normalized(request.uri().path()).trim_end_matches('/');
    let endpoint = normalized(c.resource.path()).trim_end_matches('/');
    let metadata = format!(
        "{}/.well-known/oauth-protected-resource",
        c.resource.as_str().trim_end_matches('/')
    );
    if path == "/.well-known/oauth-protected-resource"
        || path == "/.well-known/oauth-protected-resource/mcp"
        || path == format!("{endpoint}/.well-known/oauth-protected-resource")
        || path == format!("/.well-known/oauth-protected-resource{endpoint}")
    {
        return response(
            StatusCode::OK,
            json!({"resource":c.resource.as_str().trim_end_matches('/'),"authorization_servers":[c.issuer],"bearer_methods_supported":["header"],"scopes_supported":["openid","email","profile","offline_access"],"resource_name":"Civic Pulse","resource_documentation":"https://www.civpulse.org"}),
        );
    }
    if path.is_empty() {
        return response(
            StatusCode::OK,
            json!({"name":"Civic Pulse MCP","mcp_endpoint":c.resource.as_str(),"authorization_server":c.issuer,"protected_resource_metadata":metadata}),
        );
    }
    if path == "/healthz" {
        return response(StatusCode::OK, json!({"status":"ok"}));
    }
    if path != endpoint {
        return StatusCode::NOT_FOUND.into_response();
    }
    let unauthorized = |invalid: bool| {
        let mut r = response(
            StatusCode::UNAUTHORIZED,
            json!({"error":if invalid{"invalid_token"}else{"unauthorized"},"error_description":"A valid Supabase user access token is required."}),
        );
        r.headers_mut()
            .insert("cache-control", HeaderValue::from_static("no-store"));
        if let Ok(header) = HeaderValue::from_str(&format!(
            "Bearer resource_metadata=\"{metadata}\"{}",
            if invalid {
                ", error=\"invalid_token\""
            } else {
                ""
            }
        )) {
            r.headers_mut().insert("www-authenticate", header);
        }
        r
    };
    let auth = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let parts: Vec<_> = auth.split_whitespace().collect();
    let token = match parts.as_slice() {
        [scheme, token] if scheme.eq_ignore_ascii_case("bearer") => *token,
        _ => return unauthorized(!auth.is_empty()),
    };
    let backend = match authenticate(app, token).await {
        Ok(Some(b)) => b,
        Ok(None) => return unauthorized(true),
        Err(_) => {
            let mut r = response(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"error":"temporarily_unavailable"}),
            );
            r.headers_mut()
                .insert("cache-control", HeaderValue::from_static("no-store"));
            r.headers_mut()
                .insert("retry-after", HeaderValue::from_static("5"));
            return r;
        }
    };
    if request.method() != Method::POST {
        let mut r = StatusCode::METHOD_NOT_ALLOWED.into_response();
        r.headers_mut()
            .insert("allow", HeaderValue::from_static("POST"));
        return r;
    }
    if let Some(version) = request
        .headers()
        .get("mcp-protocol-version")
        .and_then(|v| v.to_str().ok())
    {
        if !VERSIONS.contains(&version) {
            return response(
                StatusCode::BAD_REQUEST,
                json!({"error":"Unsupported MCP protocol version"}),
            );
        }
    }
    let accept = request
        .headers()
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !accept.contains("application/json") || !accept.contains("text/event-stream") {
        return response(
            StatusCode::NOT_ACCEPTABLE,
            json!({"error":"Accept must include application/json and text/event-stream"}),
        );
    }
    if request
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_none_or(|s| s.split(';').next() != Some("application/json"))
    {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let Ok(body) = to_bytes(request.into_body(), 40 * 1024 * 1024).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let message: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return response(
                StatusCode::BAD_REQUEST,
                rpc_error(&Value::Null, -32700, "Parse error"),
            );
        }
    };
    if message["jsonrpc"] != "2.0"
        || !message["method"].is_string()
        || message
            .get("id")
            .is_some_and(|id| !id.is_string() && !id.is_number() && !id.is_null())
    {
        return response(
            StatusCode::BAD_REQUEST,
            rpc_error(&Value::Null, -32600, "Invalid Request"),
        );
    }
    let Some(id) = message.get("id").cloned() else {
        return StatusCode::ACCEPTED.into_response();
    };
    let params = &message["params"];
    let result = match text(&message, "method") {
        "initialize" => {
            let requested = text(params, "protocolVersion");
            json!({"protocolVersion":if VERSIONS.contains(&requested){requested}else{VERSIONS[0]},"capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"Civic Pulse","version":"1.5.0"}})
        }
        "ping" => json!({}),
        "tools/list" => {
            if params.get("cursor").is_some_and(|v| !v.is_null()) {
                return response(StatusCode::OK, rpc_error(&id, -32602, "Invalid cursor"));
            }
            json!({"tools":catalog::tools()})
        }
        "tools/call" => {
            let name = text(params, "name");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            if !catalog::tools().iter().any(|t| t["name"] == name) {
                return response(StatusCode::OK, rpc_error(&id, -32602, "Unknown tool"));
            }
            match handlers::call(&backend, name, args).await {
                Ok(value) => {
                    json!({"content":[{"type":"text","text":serde_json::to_string_pretty(&value).expect("JSON value")}]})
                }
                Err(e) => {
                    json!({"content":[{"type":"text","text":format!("Error: {e}")}],"isError":true})
                }
            }
        }
        _ => return response(StatusCode::OK, rpc_error(&id, -32601, "Method not found")),
    };
    response(
        StatusCode::OK,
        json!({"jsonrpc":"2.0","id":id,"result":result}),
    )
}
const VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
fn rpc_error(id: &Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
async fn authenticate(app: &App, token: &str) -> Result<Option<Backend>> {
    let c = &app.config;
    let response = app
        .client
        .get(c.supabase.join("/auth/v1/user")?)
        .header("apikey", &c.public_key)
        .bearer_auth(token)
        .send()
        .await?;
    if response.status().is_server_error() {
        bail!("Auth unavailable");
    }
    if !response.status().is_success() {
        return Ok(None);
    }
    let user: Value = response.json().await?;
    if user["is_anonymous"] == true || !user["id"].is_string() {
        return Ok(None);
    }
    let Ok(header) = decode_header(token) else {
        return Ok(None);
    };
    // For legacy symmetric tokens, the live Auth endpoint performs signature validation.
    // Asymmetric tokens are also independently verified against the server's JWKS.
    let claims = if header.alg == Algorithm::HS256 {
        match token
            .split('.')
            .nth(1)
            .and_then(|s| URL_SAFE_NO_PAD.decode(s).ok())
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        {
            Some(c) => c,
            None => return Ok(None),
        }
    } else {
        if !matches!(header.alg, Algorithm::RS256 | Algorithm::ES256) {
            return Ok(None);
        }
        let response = app
            .client
            .get(c.supabase.join("/auth/v1/.well-known/jwks.json")?)
            .header("apikey", &c.public_key)
            .send()
            .await?;
        if !response.status().is_success() {
            bail!("Signing keys unavailable");
        }
        let keys: JwkSet = response.json().await?;
        let Some(key) = header.kid.as_deref().and_then(|kid| keys.find(kid)) else {
            return Ok(None);
        };
        let mut validation = Validation::new(header.alg);
        validation.set_audience(&["authenticated"]);
        validation.set_issuer(&[&c.issuer]);
        validation.leeway = 0;
        match decode::<Value>(token, &DecodingKey::from_jwk(key)?, &validation) {
            Ok(data) => data.claims,
            Err(_) => return Ok(None),
        }
    };
    let audience = claims["aud"] == "authenticated"
        || claims["aud"]
            .as_array()
            .is_some_and(|v| v.iter().any(|x| x == "authenticated"));
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    if claims["iss"] != c.issuer
        || !audience
        || claims["role"] != "authenticated"
        || claims["is_anonymous"] == true
        || claims["sub"] != user["id"]
        || claims["exp"].as_u64().is_none_or(|exp| exp <= now)
    {
        return Ok(None);
    }
    Ok(Some(Backend {
        client: app.client.clone(),
        base: c.supabase.clone(),
        key: c.public_key.clone(),
        token: token.into(),
        user,
        kb: c.kb.clone(),
        georgia: c.georgia.clone(),
    }))
}
