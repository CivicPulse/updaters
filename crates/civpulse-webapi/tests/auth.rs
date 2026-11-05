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
