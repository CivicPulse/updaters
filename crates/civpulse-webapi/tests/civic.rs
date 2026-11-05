use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
const OWNER: &str = "00000000-0000-4000-8000-000000000001";
const OTHER: &str = "00000000-0000-4000-8000-000000000002";
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<Value>>>);
async fn upstream(State(log): State<Log>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let method = request.method().as_str().to_owned();
    let auth = request
        .headers()
        .get(header::AUTHORIZATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(request.headers()["apikey"], "sb_publishable_test");
    let query: Value = url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
        .map(|(k, v)| (k.into_owned(), json!(v)))
        .collect();
    let body: Value = serde_json::from_slice(
        &to_bytes(request.into_body(), 40 * 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap_or(Value::Null);
    log.0
        .lock()
        .unwrap()
        .push(json!({"path":path,"method":method,"auth":auth,"query":query,"body":body}));
    if path == "/auth/v1/user" {
        return Json(json!({"id":OWNER,"email":"test@example.invalid","is_anonymous":auth=="Bearer anonymous"})).into_response();
    }
    if path == "/rest/v1/rpc/is_admin" {
        return Json(json!(auth == "Bearer admin")).into_response();
    }
    if path == "/rest/v1/rpc/search_site" {
        assert_eq!(body["p_type"], "event");
        return Json(json!([{"id":OWNER,"name":"Visible event"}])).into_response();
    }
    if path == "/rest/v1/rpc/save_records_request" {
        if body["p_id"].is_null() {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"message":"private database detail"})),
            )
                .into_response();
        }
        return Json(json!(OWNER)).into_response();
    }
    if path == "/rest/v1/profiles" && method == "PATCH" {
        return Json(if query["id"] == format!("eq.{OWNER}") {
            json!([{"id":OWNER,"display_name":"Updated"}])
        } else {
            json!([])
        })
        .into_response();
    }
    if path == "/rest/v1/people" {
        if auth == "Bearer admin" {
            return Json(json!([{"id":OWNER,"full_name":"Test"}])).into_response();
        }
        if method == "POST" {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"message":"private database detail"})),
            )
                .into_response();
        }
        return Json(json!([])).into_response();
    }
    panic!("Unexpected backend request: {method} {path}");
}
async fn app() -> (Router, Log, tokio::task::JoinHandle<()>) {
    let log = Log::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend = civpulse_webapi::Backend::new(
        &format!("http://{}/", listener.local_addr().unwrap()),
        "sb_publishable_test",
    )
    .unwrap();
    let upstream = Router::new().fallback(upstream).with_state(log.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    (civpulse_webapi::router(backend), log, task)
}
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(
            req.body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let status = response.status();
    let body = serde_json::from_slice(
        &to_bytes(response.into_body(), 10 * 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap_or(Value::Null);
    (status, body)
}
#[tokio::test]
async fn every_civic_surface_rejects_missing_and_anonymous_identity() {
    let (app, _, task) = app().await;
    for token in [None, Some("anonymous")] {
        for path in [
            "/api/v1/operations",
            "/api/v1/operations/list_civic_resources",
            "/api/v1/civic-resources",
            "/api/v1/data/people",
            "/api/v1/data/people/00000000-0000-4000-8000-000000000001",
        ] {
            for method in ["GET", "POST", "PATCH", "DELETE"] {
                assert_eq!(
                    call(&app, method, path, token, json!({})).await.0,
                    StatusCode::UNAUTHORIZED
                );
            }
        }
    }
    task.abort();
}
#[tokio::test]
async fn api_exposes_the_exact_mcp_catalog_and_reviewed_resources() {
    let (app, _, task) = app().await;
    let (status, body) = call(&app, "GET", "/api/v1/operations", Some("member"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["operations"], json!(civpulse_civic::catalog::tools()));
    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/civic-resources",
        Some("member"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, civpulse_civic::resources::manifest().clone());
    for tool in civpulse_civic::catalog::tools() {
        let (status, body) = call(
            &app,
            "GET",
            &format!("/api/v1/operations/{}", tool["name"].as_str().unwrap()),
            Some("member"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, *tool);
    }
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/operations/arbitrary_sql",
            Some("member"),
            json!({})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    task.abort();
}
#[tokio::test]
async fn civic_crud_forwards_caller_rls_and_never_claims_filtered_writes_succeeded() {
    let (app, log, task) = app().await;
    let create = json!({"slug":"test","full_name":"Test"});
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/data/people",
            Some("member"),
            create.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "POST", "/api/v1/data/people", Some("admin"), create)
            .await
            .0,
        StatusCode::OK
    );
    for token in ["member", "admin"] {
        let want = if token == "admin" {
            StatusCode::OK
        } else {
            StatusCode::NOT_FOUND
        };
        assert_eq!(
            call(
                &app,
                "PATCH",
                "/api/v1/data/people",
                Some(token),
                json!({"keys":{"id":OWNER},"data":{"full_name":"Test"}})
            )
            .await
            .0,
            want
        );
        assert_eq!(
            call(
                &app,
                "DELETE",
                "/api/v1/data/people",
                Some(token),
                json!({"keys":{"id":OWNER}})
            )
            .await
            .0,
            want
        );
    }
    assert_eq!(
        call(
            &app,
            "PATCH",
            "/api/v1/data/profiles",
            Some("member"),
            json!({"keys":{"id":OWNER},"data":{"display_name":"Updated"}})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "PATCH",
            "/api/v1/data/profiles",
            Some("member"),
            json!({"keys":{"id":OTHER},"data":{"display_name":"Updated"}})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/data/people",
        Some("member"),
        json!({"slug":"test","full_name":"Test"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(!body.to_string().contains("private database detail"));
    assert!(
        log.0
            .lock()
            .unwrap()
            .iter()
            .all(|r| !r["auth"].as_str().unwrap().contains("service"))
    );
    task.abort();
}
#[tokio::test]
async fn core_discovery_owner_workflows_and_immutable_evidence_keep_existing_gates() {
    let (app, log, task) = app().await;
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/data/events?query=meeting",
            Some("member"),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/operations/save_records_request",
            Some("member"),
            json!({"id":OWNER,"values":{"title":"Owner edit"}})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/operations/save_records_request",
            Some("member"),
            json!({"values":{"title":"Forbidden create"}})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    for operation in ["update_civic_data", "delete_civic_data"] {
        assert_eq!(call(&app,"POST",&format!("/api/v1/operations/{operation}"),Some("admin"),if operation=="update_civic_data"{json!({"resource":"structured_turnout_reports","keys":{"id":OWNER},"data":{"document":"forged"}})}else{json!({"resource":"structured_turnout_reports","keys":{"id":OWNER}})}).await.0,StatusCode::FORBIDDEN);
    }
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/operations/get_voter_roll_history",
            Some("member"),
            json!({"registration_number":"123"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert!(
        !log.0
            .lock()
            .unwrap()
            .iter()
            .any(|r| r["path"] == "/rest/v1/rpc/get_voter_roll_history")
    );
    task.abort();
}

#[tokio::test]
async fn admin_only_evidence_is_rejected_before_expensive_table_scans() {
    let (app, log, task) = app().await;
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/operations/search_civic_data",
            Some("member"),
            json!({"resource":"sos_result_items","limit":1})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert!(
        !log.0
            .lock()
            .unwrap()
            .iter()
            .any(|request| request["path"] == "/rest/v1/sos_result_items")
    );
    task.abort();
}
