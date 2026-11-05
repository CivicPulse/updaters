use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::Request,
    http::{StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use civpulse_webapi::{Backend, router};
use serde_json::json;
use tower::ServiceExt;

async fn app() -> (Router, tokio::task::JoinHandle<()>) {
    let upstream = Router::new()
        .route(
            "/auth/v1/user",
            get(|request: Request| async move {
                assert_eq!(request.headers()["apikey"], "sb_publishable_test");
                match request.headers()[header::AUTHORIZATION].to_str().unwrap() {
                    "Bearer member" | "Bearer admin" | "Bearer denied" => {
                        Json(json!({"id":"user-1"})).into_response()
                    }
                    "Bearer anonymous" => {
                        Json(json!({"id":"guest","is_anonymous":true})).into_response()
                    }
                    "Bearer outage" => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                    _ => StatusCode::UNAUTHORIZED.into_response(),
                }
            }),
        )
        .route(
            "/rest/v1/organizations",
            get(|request: Request| async move {
                assert_eq!(request.headers()["apikey"], "sb_publishable_test");
                assert!(request.uri().query().unwrap().contains("limit=100"));
                match request.headers()[header::AUTHORIZATION].to_str().unwrap() {
                    "Bearer member" => Json(json!([{"id":"visible-to-member"}])).into_response(),
                    "Bearer admin" => Json(json!([{"id":"visible-to-admin"}])).into_response(),
                    "Bearer denied" => StatusCode::FORBIDDEN.into_response(),
                    _ => panic!("unvalidated credentials reached data API"),
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend = Backend::new(
        &format!("http://{}", listener.local_addr().unwrap()),
        "sb_publishable_test",
    )
    .unwrap();
    let task = tokio::spawn(async { axum::serve(listener, upstream).await.unwrap() });
    (router(backend), task)
}

#[tokio::test]
async fn all_paths_and_methods_require_authentication() {
    let (app, task) = app().await;
    for path in ["/api/v1/me", "/api/v1/organizations", "/health", "/missing"] {
        for method in ["GET", "POST", "OPTIONS"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .method(method)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
            assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        }
    }
    task.abort();
}

#[tokio::test]
async fn rejects_invalid_anonymous_and_duplicate_credentials_and_fails_closed() {
    let (app, task) = app().await;
    for (token, status) in [
        ("Basic member", StatusCode::UNAUTHORIZED),
        ("Bearer forged", StatusCode::UNAUTHORIZED),
        ("Bearer anonymous", StatusCode::UNAUTHORIZED),
        ("Bearer outage", StatusCode::SERVICE_UNAVAILABLE),
        ("Bearer ", StatusCode::UNAUTHORIZED),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/me")
                    .header(header::AUTHORIZATION, token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/me")
                .header(header::AUTHORIZATION, "Bearer member")
                .header(header::AUTHORIZATION, "Bearer admin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    task.abort();
}

#[tokio::test]
async fn preserves_user_context_and_database_permission_denials() {
    let (app, task) = app().await;
    for (token, expected) in [
        ("member", "visible-to-member"),
        ("admin", "visible-to-admin"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/organizations")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            json!([{"id":expected}])
        );
    }
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/organizations")
                .header(header::AUTHORIZATION, "Bearer denied")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    task.abort();
}

#[test]
fn rejects_privileged_keys_and_unsafe_origins() {
    for origin in [
        "http://example.org",
        "https://example.org/path",
        "https://user@example.org",
        "https://example.org/?q=x",
    ] {
        assert!(Backend::new(origin, "sb_publishable_test").is_err());
    }
    for key in ["sb_secret_test", "eyJservice-role", ""] {
        assert!(Backend::new("https://example.org", key).is_err());
    }
}

#[tokio::test]
async fn result_routes_authenticate_and_validate_before_source_access() {
    let (app, task) = app().await;
    for (uri, auth, status) in [
        (
            "/api/v1/elections/295d864d-80c8-4a87-ab16-3f4f3fa5ddb7/results",
            None,
            StatusCode::UNAUTHORIZED,
        ),
        (
            "/api/v1/elections/invalid/results",
            Some("Bearer member"),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/elections/295d864d-80c8-4a87-ab16-3f4f3fa5ddb7/results",
            Some("Bearer member"),
            StatusCode::BAD_GATEWAY,
        ),
    ] {
        let mut request = Request::builder().uri(uri);
        if let Some(auth) = auth {
            request = request.header(header::AUTHORIZATION, auth);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }
    task.abort();
}

#[tokio::test]
async fn public_documentation_describes_secured_api() {
    // An unreachable identity backend proves documentation needs no Auth service.
    let app = router(Backend::new("http://127.0.0.1:1", "sb_publishable_test").unwrap());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    let spec: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(spec["openapi"], "3.1.0");
    assert_eq!(spec["security"], json!([{"bearerAuth":[]}]));
    assert_eq!(
        spec["components"]["securitySchemes"]["bearerAuth"]["scheme"],
        "bearer"
    );
    for (path, methods) in [
        ("/api/v1/me", vec!["get"]),
        ("/api/v1/operations", vec!["get"]),
        ("/api/v1/operations/{operation}", vec!["get", "post"]),
        ("/api/v1/civic-resources", vec!["get"]),
        (
            "/api/v1/data/{resource}",
            vec!["get", "post", "patch", "delete"],
        ),
        ("/api/v1/data/{resource}/{id}", vec!["get"]),
        ("/api/v1/organizations", vec!["get"]),
        ("/api/v1/elections", vec!["get"]),
        ("/api/v1/people", vec!["get"]),
        ("/api/v1/elections/{election_id}/results", vec!["get"]),
    ] {
        for method in methods {
            assert!(
                spec["paths"][path][method]["responses"]["200"].is_object(),
                "{method} {path}"
            );
        }
    }
    assert_documented_constraints(&spec);
    for method in ["patch", "delete"] {
        let schema = &spec["paths"]["/api/v1/data/{resource}"][method]["requestBody"]["content"]["application/json"]
            ["schema"];
        assert!(schema["properties"]["keys"].is_object());
        assert!(schema["properties"].get("resource").is_none());
        assert!(
            !schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("resource"))
        );
    }
    for path in ["/api/v1/docs", "/api/v1/docs/"] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        let bytes = to_bytes(response.into_body(), 10000).await.unwrap();
        let html = std::str::from_utf8(&bytes).unwrap();
        assert!(html.contains("SwaggerUIBundle"));
        assert!(html.contains("/api/v1/openapi.json"));
        assert!(html.contains("persistAuthorization: false"));
    }
    for path in ["/api/v1/docs", "/api/v1/openapi.json"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/operations")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

fn assert_documented_constraints(spec: &serde_json::Value) {
    let parameters = spec["paths"]["/api/v1/data/{resource}"]["get"]["parameters"]
        .as_array()
        .unwrap();
    for name in ["query", "limit", "offset"] {
        let documented = &parameters.iter().find(|p| p["name"] == name).unwrap()["schema"];
        let search = civpulse_civic::catalog::tools()
            .iter()
            .find(|t| t["name"] == "search_civic_data")
            .unwrap();
        assert_eq!(documented, &search["inputSchema"]["properties"][name]);
    }
    for code in ["400", "413", "415", "422"] {
        assert_eq!(
            spec["paths"]["/api/v1/data/{resource}"]["post"]["responses"][code]["content"]["text/plain"]
                ["schema"]["type"],
            "string"
        );
    }
}
