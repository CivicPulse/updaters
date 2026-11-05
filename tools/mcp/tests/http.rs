use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{StatusCode, header},
    response::IntoResponse,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use civpulse_mcp::{
    catalog,
    server::{Config, router},
};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use ring::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair},
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tower::ServiceExt;
const ID: &str = "00000000-0000-4000-8000-000000000001";
const SECRET: &[u8] = b"local-test-signing-secret-not-a-production-credential";
struct Auth {
    signing_key: EncodingKey,
    verifying_key: DecodingKey,
    jwks: Value,
    accept_signature: AtomicBool,
    revoked: AtomicBool,
    admin: AtomicBool,
    calls: Mutex<Vec<String>>,
}
impl Auth {
    fn new() -> Self {
        let rng = SystemRandom::new();
        let der = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let key =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, der.as_ref(), &rng).unwrap();
        let public = key.public_key().as_ref();
        Self {
            signing_key: EncodingKey::from_ec_der(der.as_ref()),
            verifying_key: DecodingKey::from_ec_der(public),
            jwks: json!({"keys":[{"kty":"EC","kid":"test-key","use":"sig","alg":"ES256","crv":"P-256","x":URL_SAFE_NO_PAD.encode(&public[1..33]),"y":URL_SAFE_NO_PAD.encode(&public[33..65])}]}),
            accept_signature: AtomicBool::new(false),
            revoked: AtomicBool::new(false),
            admin: AtomicBool::new(false),
            calls: Mutex::new(vec![]),
        }
    }
}

async fn upstream(State(state): State<Arc<Auth>>, request: Request) -> axum::response::Response {
    let path = request.uri().path().to_owned();
    state.calls.lock().unwrap().push(path.clone());
    assert_eq!(request.headers()["apikey"], "test-public-key");
    let token = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .strip_prefix("Bearer ")
        .unwrap_or("");
    if path == "/auth/v1/.well-known/jwks.json" {
        return axum::Json(state.jwks.clone()).into_response();
    }
    let algorithm =
        jsonwebtoken::decode_header(token).map_or(jsonwebtoken::Algorithm::HS256, |h| h.alg);
    let mut validation = Validation::new(algorithm);
    validation.set_audience(&["authenticated"]);
    if path == "/auth/v1/user" {
        if state.revoked.load(Ordering::SeqCst)
            || (!state.accept_signature.load(Ordering::SeqCst)
                && decode::<Value>(
                    token,
                    &if algorithm == jsonwebtoken::Algorithm::ES256 {
                        state.verifying_key.clone()
                    } else {
                        DecodingKey::from_secret(SECRET)
                    },
                    &validation,
                )
                .is_err())
        {
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(json!({"error":"invalid_token"})),
            )
                .into_response();
        }
        return axum::Json(json!({"id":ID,"email":"test@example.org","is_anonymous":false}))
            .into_response();
    }
    if path == "/rest/v1/rpc/is_admin" {
        return axum::Json(json!(state.admin.load(Ordering::SeqCst))).into_response();
    }
    axum::Json(json!({"status":"ok"})).into_response()
}
struct Harness {
    app: Router,
    auth: Arc<Auth>,
    task: tokio::task::JoinHandle<()>,
    issuer: String,
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Harness {
    async fn new() -> Self {
        let auth = Arc::new(Auth::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let upstream = Router::new().fallback(upstream).with_state(auth.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });
        let issuer = format!("{base}/auth/v1");
        let app = router(Config {
            listen: "127.0.0.1:0".into(),
            supabase: base.parse().unwrap(),
            public_key: "test-public-key".into(),
            resource: "http://localhost:8789/functions/v1/mcp/v2".parse().unwrap(),
            issuer: issuer.clone(),
            kb: None,
            georgia: None,
        })
        .unwrap();
        Self {
            app,
            auth,
            task,
            issuer,
        }
    }
    fn token(&self, patch: Value) -> String {
        let mut claims = json!({"sub":ID,"aud":"authenticated","iss":self.issuer,"role":"authenticated","exp":chrono::Utc::now().timestamp()+600});
        if let Value::Object(patch) = patch {
            for (k, v) in patch {
                claims[k] = v;
            }
        }
        encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(SECRET),
        )
        .unwrap()
    }
    async fn request(
        &self,
        path: &str,
        method: &str,
        body: Value,
        auth: Option<&str>,
        origin: Option<&str>,
    ) -> (StatusCode, axum::http::HeaderMap, Value) {
        let mut request = Request::builder()
            .uri(path)
            .method(method)
            .header("host", "localhost:8789")
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        if let Some(token) = auth {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        let r = self
            .app
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = r.status();
        let headers = r.headers().clone();
        let bytes = to_bytes(r.into_body(), 40 * 1024 * 1024).await.unwrap();
        (
            status,
            headers,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
}
fn rpc(method: &str, params: Value) -> Value {
    {
        let mut result = json!({"jsonrpc":"2.0","id":1,"method":method});
        result["params"] = params;
        result
    }
}
#[tokio::test]
async fn discovery_challenge_cors_and_host() {
    let h = Harness::new().await;
    let (status, headers, _) = h
        .request(
            "/mcp/v2",
            "POST",
            json!({}),
            None,
            Some("https://www.civpulse.org"),
        )
        .await;
    assert_eq!(status, 401);
    assert!(
        headers[header::WWW_AUTHENTICATE]
            .to_str()
            .unwrap()
            .contains("/mcp/v2/.well-known/oauth-protected-resource")
    );
    assert_eq!(
        headers["access-control-allow-origin"],
        "https://www.civpulse.org"
    );
    let (status, _, metadata) = h
        .request(
            "/functions/v1/mcp/v2/.well-known/oauth-protected-resource",
            "GET",
            Value::Null,
            None,
            None,
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        metadata["resource"],
        "http://localhost:8789/functions/v1/mcp/v2"
    );
    assert!(h.auth.calls.lock().unwrap().is_empty());
    assert_eq!(
        h.request(
            "/mcp/v2",
            "OPTIONS",
            Value::Null,
            None,
            Some("https://evil.example")
        )
        .await
        .0,
        403
    );
    assert_eq!(
        h.request(
            "/mcp/v2",
            "OPTIONS",
            Value::Null,
            None,
            Some("https://civpulse.org")
        )
        .await
        .0,
        204
    );
    let request = Request::builder()
        .uri("/healthz")
        .header("host", "evil.example")
        .body(Body::empty())
        .unwrap();
    assert_eq!(h.app.clone().oneshot(request).await.unwrap().status(), 403);
    assert_eq!(
        h.request("/mcp", "GET", Value::Null, None, None).await.0,
        404
    );
}
#[tokio::test]
async fn handshake_catalog_notifications_and_revocation() {
    let h = Harness::new().await;
    let token = h.token(json!({}));
    let (status, _, body) = h
        .request(
            "/functions/v1/mcp/v2/",
            "POST",
            rpc("initialize", json!({"protocolVersion":"2025-11-25"})),
            Some(&token),
            None,
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["result"]["protocolVersion"], "2025-11-25");
    let (_, _, body) = h
        .request(
            "/mcp/v2",
            "POST",
            rpc("tools/list", json!({})),
            Some(&token),
            None,
        )
        .await;
    assert_eq!(body["result"]["tools"], json!(catalog::tools()));
    assert_eq!(
        h.request(
            "/mcp/v2",
            "POST",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            Some(&token),
            None
        )
        .await
        .0,
        202
    );
    assert_eq!(
        h.request("/mcp/v2", "GET", Value::Null, Some(&token), None)
            .await
            .0,
        405
    );
    h.auth.revoked.store(true, Ordering::SeqCst);
    assert_eq!(
        h.request(
            "/mcp/v2",
            "POST",
            rpc("ping", json!({})),
            Some(&token),
            None
        )
        .await
        .0,
        401
    );
}
#[tokio::test]
async fn claims_and_signature_rejections() {
    let h = Harness::new().await;
    for patch in [
        json!({"iss":"https://wrong.example/auth"}),
        json!({"role":"service_role"}),
        json!({"sub":"00000000-0000-4000-8000-000000000002"}),
        json!({"exp":1}),
        json!({"aud":"anon"}),
        json!({"is_anonymous":true}),
    ] {
        let token = h.token(patch.clone());
        assert_eq!(
            h.request(
                "/mcp/v2",
                "POST",
                rpc("ping", json!({})),
                Some(&token),
                None
            )
            .await
            .0,
            401,
            "{patch}"
        );
    }
    let mut token = h.token(json!({}));
    token.push('x');
    assert_eq!(
        h.request(
            "/mcp/v2",
            "POST",
            rpc("ping", json!({})),
            Some(&token),
            None
        )
        .await
        .0,
        401
    );
    let token = h.token(json!({"aud":["other","authenticated"]}));
    assert_eq!(
        h.request(
            "/mcp/v2",
            "POST",
            rpc("ping", json!({})),
            Some(&token),
            None
        )
        .await
        .0,
        200
    );
}
#[tokio::test]
async fn admin_gate_blocks_writes_and_private_reads() {
    let h = Harness::new().await;
    let token = h.token(json!({}));
    for (name, args) in [
        (
            "save_person",
            json!({"request":{"operation":"create","slug":"sample","full_name":"Sample"}}),
        ),
        (
            "read_related_records",
            json!({"request":{"relation":"map_layer_links","map_id":ID}}),
        ),
        ("promote_user_to_admin", json!({"user_id":ID})),
    ] {
        let (_, _, body) = h
            .request(
                "/mcp/v2",
                "POST",
                rpc("tools/call", json!({"name":name,"arguments":args})),
                Some(&token),
                None,
            )
            .await;
        assert_eq!(body["result"]["isError"], true);
        assert!(
            body["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Admin access required")
        );
    }
    assert!(
        h.auth
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|p| p == "/auth/v1/user" || p == "/rest/v1/rpc/is_admin")
    );
    let (_, _, body) = h
        .request(
            "/mcp/v2",
            "POST",
            rpc(
                "tools/call",
                json!({"name":"school_finances","arguments":{"operation":{"action":"browse"}}}),
            ),
            Some(&token),
            None,
        )
        .await;
    assert!(body["result"].get("isError").is_none());
    assert!(
        h.auth
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|p| p == "/rest/v1/rpc/browse_school_finances")
    );
}
#[tokio::test]
async fn malformed_protocol_and_schema_fail_without_mutation() {
    let h = Harness::new().await;
    h.auth.admin.store(true, Ordering::SeqCst);
    let token = h.token(json!({}));
    let(_,_,body)=h.request("/mcp/v2","POST",rpc("tools/call",json!({"name":"save_person","arguments":{"request":{"operation":"update","id":ID,"unknown":true}}})),Some(&token),None).await;
    assert_eq!(body["result"]["isError"], true);
    assert!(
        h.auth
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|p| p == "/auth/v1/user")
    );
    let (_, _, body) = h
        .request(
            "/mcp/v2",
            "POST",
            rpc("unknown", json!({})),
            Some(&token),
            None,
        )
        .await;
    assert_eq!(body["error"]["code"], -32601);
    let request = Request::builder()
        .method("POST")
        .uri("/mcp/v2")
        .header("host", "localhost")
        .header("authorization", format!("Bearer {token}"))
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .body(Body::from("{"))
        .unwrap();
    assert_eq!(h.app.clone().oneshot(request).await.unwrap().status(), 400);
    let request = Request::builder()
        .method("POST")
        .uri("/mcp/v2")
        .header("host", "localhost")
        .header("authorization", format!("Bearer {token}"))
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .body(Body::from(rpc("ping", json!({})).to_string()))
        .unwrap();
    assert_eq!(h.app.clone().oneshot(request).await.unwrap().status(), 406);
}

#[tokio::test]
async fn asymmetric_tokens_verify_jwks_independently() {
    let h = Harness::new().await;
    let claims = json!({"sub":ID,"aud":"authenticated","iss":h.issuer,"role":"authenticated","exp":chrono::Utc::now().timestamp()+600});
    let mut header = Header::new(jsonwebtoken::Algorithm::ES256);
    header.kid = Some("test-key".into());
    let key = &h.auth.signing_key;
    let token = encode(&header, &claims, key).unwrap();
    assert_eq!(
        h.request(
            "/mcp/v2",
            "POST",
            rpc("ping", json!({})),
            Some(&token),
            None
        )
        .await
        .0,
        200
    );
    assert!(
        h.auth
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|p| p == "/auth/v1/.well-known/jwks.json")
    );
    // Even an upstream returning a user cannot bypass independent asymmetric verification.
    h.auth.accept_signature.store(true, Ordering::SeqCst);
    let (prefix, signature) = token.rsplit_once('.').unwrap();
    let bad = format!(
        "{prefix}.{}{}",
        if signature.starts_with('A') { "B" } else { "A" },
        &signature[1..]
    );
    assert_eq!(
        h.request("/mcp/v2", "POST", rpc("ping", json!({})), Some(&bad), None)
            .await
            .0,
        401
    );
    header.kid = Some("unknown-key".into());
    let token = encode(&header, &claims, key).unwrap();
    assert_eq!(
        h.request(
            "/mcp/v2",
            "POST",
            rpc("ping", json!({})),
            Some(&token),
            None
        )
        .await
        .0,
        401
    );
}

#[tokio::test]
async fn path_scoped_oauth_discovery_and_responses_disable_caching() {
    let h = Harness::new().await;
    for path in [
        "/mcp/v2/.well-known/oauth-protected-resource",
        "/.well-known/oauth-protected-resource/mcp/v2",
    ] {
        let (status, headers, body) = h.request(path, "GET", Value::Null, None, None).await;
        assert_eq!(status, 200);
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(
            body["resource"],
            "http://localhost:8789/functions/v1/mcp/v2"
        );
    }
    let token = h.token(json!({}));
    let (status, headers, _) = h
        .request(
            "/mcp/v2",
            "POST",
            rpc("tools/list", json!({})),
            Some(&token),
            None,
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(headers["cache-control"], "no-store");
}
