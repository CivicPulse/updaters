use crate::{backend::Backend, catalog, handlers};
use axum::{
    Router,
    body::to_bytes,
    extract::{Request, State},
    response::IntoResponse,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
struct Mock {
    queue: Arc<Mutex<VecDeque<Value>>>,
    errors: Arc<Mutex<Vec<String>>>,
}
async fn mock(State(state): State<Mock>, request: Request) -> axum::response::Response {
    let expected = state.queue.lock().unwrap().pop_front();
    let Some(expected) = expected else {
        state
            .errors
            .lock()
            .unwrap()
            .push(format!("Unexpected request {}", request.uri()));
        return axum::Json(json!({"message":"Unexpected request"})).into_response();
    };
    let method = request.method().as_str().to_owned();
    let path = request.uri().path().to_owned();
    let mut query: Value =
        url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
            .map(|(k, v)| (k.into_owned(), Value::String(v.into_owned())))
            .collect();
    let mut want_query = expected["query"].clone();
    for q in [&mut query, &mut want_query] {
        if let Some(select) = q["select"].as_str() {
            q["select"] = json!(select.replace(' ', ""));
        }
    }
    let auth = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let key = request
        .headers()
        .get("apikey")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let bytes = to_bytes(request.into_body(), 40 * 1024 * 1024)
        .await
        .unwrap();
    let body: Value = if expected.get("body_base64").is_some() {
        json!(STANDARD.encode(&bytes))
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    let actual = json!({"method":method,"path":path,"query":query,"body":body});
    let want = json!({"method":expected["method"],"path":expected["path"],"query":want_query,"body":expected.get("body_base64").unwrap_or(&expected["body"])});
    if actual != want {
        state
            .errors
            .lock()
            .unwrap()
            .push(format!("Wire mismatch:\nactual {actual}\nexpected {want}"));
    }
    if auth
        != (if expected["no_auth"] == true {
            ""
        } else {
            "Bearer caller-token"
        })
        || key
            != (if expected["no_auth"] == true || expected["no_apikey"] == true {
                ""
            } else {
                "test-public-key"
            })
    {
        state
            .errors
            .lock()
            .unwrap()
            .push("Caller credentials not forwarded".into());
    }
    let mut response = if let Some(encoded) = expected["response_base64"].as_str() {
        STANDARD.decode(encoded).unwrap().into_response()
    } else {
        axum::Json(expected["response"].clone()).into_response()
    };
    if let Some(status) = expected["status"].as_u64() {
        *response.status_mut() =
            axum::http::StatusCode::from_u16(u16::try_from(status).unwrap()).unwrap();
    }
    if let Some(headers) = expected["headers"].as_object() {
        for (k, v) in headers {
            response.headers_mut().insert(
                axum::http::HeaderName::try_from(k).unwrap(),
                axum::http::HeaderValue::from_str(v.as_str().unwrap()).unwrap(),
            );
        }
    }
    response
}
#[tokio::test]
async fn v2_oracle_requests_and_results() {
    let fixtures: Vec<Value> = serde_json::from_str(include_str!("../oracle.json")).unwrap();
    let state = Mock {
        queue: Arc::new(Mutex::new(VecDeque::new())),
        errors: Arc::new(Mutex::new(vec![])),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(
        axum::serve(
            listener,
            Router::new().fallback(mock).with_state(state.clone()),
        )
        .into_future(),
    );
    let backend = Backend {
        client: reqwest::Client::new(),
        base: format!("http://{addr}").parse().unwrap(),
        key: "test-public-key".into(),
        token: "caller-token".into(),
        user: json!({"id":"00000000-0000-4000-8000-000000000001","email":"test@example.org"}),
        kb: None,
        georgia: None,
    };
    let mut failures = vec![];
    for fixture in &fixtures {
        state
            .queue
            .lock()
            .unwrap()
            .extend(fixture["requests"].as_array().unwrap().iter().cloned());
        let result = handlers::call(
            &backend,
            fixture["name"].as_str().unwrap(),
            fixture["args"].clone(),
        )
        .await;
        let mut errors = state.errors.lock().unwrap().drain(..).collect::<Vec<_>>();
        if !state.queue.lock().unwrap().is_empty() {
            errors.push(format!(
                "{} missing requests",
                state.queue.lock().unwrap().len()
            ));
            state.queue.lock().unwrap().clear();
        }
        match result {
            Ok(result)
                if result
                    == serde_json::from_str::<Value>(
                        &serde_json::to_string(&fixture["result"])
                            .unwrap()
                            .replace("http://localhost:54321", &format!("http://{addr}")),
                    )
                    .unwrap() => {}
            result => errors.push(format!("Result {result:?}, expected {}", fixture["result"])),
        }
        if !errors.is_empty() {
            failures.push(format!(
                "{} {}:\n{}",
                fixture["name"],
                fixture["args"],
                errors.join("\n")
            ));
        }
    }
    task.abort();
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    assert!(fixtures.len() >= 120);
}
#[test]
fn validation_and_defaults() {
    assert_eq!(catalog::tools().len(), 106);
    assert_eq!(
        catalog::arguments("search_site", json!({})).unwrap(),
        json!({"query":"","mode":"site","offset":0})
    );
    assert!(catalog::arguments("save_person",json!({"request":{"operation":"update","id":"00000000-0000-4000-8000-000000000001","unknown":true}})).is_err());
    assert!(
        catalog::arguments(
            "save_relationship",
            json!({"request":{"relationship":"arbitrary","change":{"operation":"create"}}})
        )
        .is_err()
    );
    assert!(catalog::arguments("search_users", json!({"offset":-1})).is_err());
    assert!(catalog::arguments("get_civic_record", json!({"type":"person","id":"bad"})).is_err());
}

async fn upload_harness() -> (Backend, Mock, tokio::task::JoinHandle<std::io::Result<()>>) {
    let state = Mock {
        queue: Arc::new(Mutex::new(VecDeque::new())),
        errors: Arc::new(Mutex::new(vec![])),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(
        axum::serve(
            listener,
            Router::new().fallback(mock).with_state(state.clone()),
        )
        .into_future(),
    );
    let backend = Backend {
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        base: format!("http://{addr}").parse().unwrap(),
        key: "test-public-key".into(),
        token: "caller-token".into(),
        user: json!({"id":ID}),
        kb: None,
        georgia: None,
    };
    (backend, state, task)
}
const ID: &str = "00000000-0000-4000-8000-000000000001";
fn upload_session() -> Value {
    json!({"id":ID,"storage_path":format!("{ID}/{ID}.txt"),"mime_type":"text/plain","size_bytes":4,"state":"pending","expires_at":"2099-01-01T00:00:00Z"})
}
fn session_read(s: &Value) -> Value {
    json!({"method":"GET","path":"/rest/v1/meeting_upload_sessions","query":{"select":"*","id":format!("eq.{ID}")},"body":null,"response":s})
}
fn transfer_rpc(action: &str, extra: Value, response: Value) -> Value {
    let mut body = json!({"p_action":action,"p_id":ID});
    if !["status", "acquire"].contains(&action) {
        body["p_lease"] = json!("lease1");
    }
    if let Value::Object(extra) = extra {
        for (k, v) in extra {
            body[k] = v;
        }
    }
    let mut result = json!({"method":"POST","path":"/rest/v1/rpc/manage_meeting_upload_transfer","query":{},"body":body});
    result["response"] = response;
    result
}
fn head_request(offset: u64) -> Value {
    json!({"method":"HEAD","path":"/storage/v1/upload/resumable/transfer1","query":{},"body":null,"response":null,"headers":{"upload-length":"4","upload-offset":offset.to_string(),"upload-metadata":format!("bucketName {},objectName {},contentType {}",STANDARD.encode("meeting-attachments"),STANDARD.encode(format!("{ID}/{ID}.txt")),STANDARD.encode("text/plain"))}})
}
fn assert_mock_done(state: &Mock) {
    let queue = state.queue.lock().unwrap();
    assert!(queue.is_empty(), "Missing backend requests: {queue:?}");
    drop(queue);
    let errors = state.errors.lock().unwrap();
    assert!(errors.is_empty(), "{errors:?}");
}
#[tokio::test]
async fn tus_lost_creation_receipt_never_reposts() {
    let (b, state, task) = upload_harness().await;
    let s = upload_session();
    let current = json!({"state":"active","offset":0,"lease":"lease1"});
    state.queue.lock().unwrap().extend([session_read(&s),transfer_rpc("status",json!({}),json!({"state":"not_started"})),transfer_rpc("acquire",json!({}),current.clone()),transfer_rpc("creating",json!({}),json!({})),json!({"method":"POST","path":"/storage/v1/upload/resumable","query":{},"body":null,"status":201,"response":{}}),transfer_rpc("release",json!({}),json!({}))]);
    let result=crate::uploads::call(&b,"manage_meeting_attachment_upload",&json!({"action":"transfer_chunk","id":ID,"offset":0,"data_base64":STANDARD.encode("data")})).await.unwrap();
    assert_eq!(result["state"], "creation_uncertain");
    assert_mock_done(&state);
    let current = json!({"state":"active","offset":0,"lease":"lease1","creating":true});
    state.queue.lock().unwrap().extend([
        session_read(&s),
        transfer_rpc("status", json!({}), current.clone()),
        transfer_rpc("acquire", json!({}), current),
        transfer_rpc("release", json!({}), json!({})),
    ]);
    let result=crate::uploads::call(&b,"manage_meeting_attachment_upload",&json!({"action":"transfer_chunk","id":ID,"offset":0,"data_base64":STANDARD.encode("data")})).await.unwrap();
    assert_eq!(result["state"], "creation_uncertain");
    assert_mock_done(&state);
    task.abort();
}
#[tokio::test]
async fn tus_expired_auth_clears_only_confirmed_precreation_rejection() {
    for confirmed in [true, false] {
        let (b, state, task) = upload_harness().await;
        let current = json!({"state":"active","offset":0,"lease":"lease1"});
        let mut queue = vec![
            session_read(&upload_session()),
            transfer_rpc("status", json!({}), json!({"state":"not_started"})),
            transfer_rpc("acquire", json!({}), current),
            transfer_rpc("creating", json!({}), json!({})),
            json!({"method":"POST","path":"/storage/v1/upload/resumable","query":{},"body":null,"status":403,"response":if confirmed{json!({"code":"AccessDenied","error":"Unauthorized","statusCode":"403","message":"\"exp\" claim timestamp check failed"})}else{json!({"error":"Forbidden"})}}),
        ];
        if confirmed {
            queue.push(transfer_rpc(
                "creation_auth_rejected",
                json!({}),
                json!({"state":"not_started","offset":0}),
            ));
        }
        queue.push(transfer_rpc("release", json!({}), json!({})));
        state.queue.lock().unwrap().extend(queue);
        let result=crate::uploads::call(&b,"manage_meeting_attachment_upload",&json!({"action":"transfer_chunk","id":ID,"offset":0,"data_base64":STANDARD.encode("data")})).await.unwrap();
        assert_eq!(
            result["state"],
            if confirmed {
                "authentication_required"
            } else {
                "creation_uncertain"
            }
        );
        assert_mock_done(&state);
        task.abort();
    }
}
#[tokio::test]
async fn tus_partial_patch_reconciles_receipts_and_replay() {
    let (b, state, task) = upload_harness().await;
    let loc = b
        .base
        .join("/storage/v1/upload/resumable/transfer1")
        .unwrap();
    let current = json!({"state":"active","offset":0,"lease":"lease1","location":loc.as_str()});
    let prepared = json!({"state":"active","offset":0,"chunk":{"committed":false}});
    let committed = json!({"state":"active","offset":4});
    state.queue.lock().unwrap().extend([session_read(&upload_session()),transfer_rpc("status",json!({}),current.clone()),transfer_rpc("acquire",json!({}),current.clone()),head_request(2),transfer_rpc("prepare",json!({"p_offset":0,"p_size":4,"p_sha256":crate::uploads::sha(b"data")}),prepared),json!({"method":"PATCH","path":"/storage/v1/upload/resumable/transfer1","query":{},"body_base64":STANDARD.encode("ta"),"status":500,"response":{}}),head_request(4),transfer_rpc("commit",json!({"p_offset":0}),committed),transfer_rpc("release",json!({}),json!({}))]);
    let args =
        json!({"action":"transfer_chunk","id":ID,"offset":0,"data_base64":STANDARD.encode("data")});
    let result = crate::uploads::call(&b, "manage_meeting_attachment_upload", &args)
        .await
        .unwrap();
    assert_eq!(result["status"], "reconciled");
    assert_eq!(result["offset"], 4);
    assert!(!result.to_string().contains("caller-token"));
    assert!(!result.to_string().contains("transfer1"));
    assert_mock_done(&state);
    state.queue.lock().unwrap().extend([
        session_read(&upload_session()),
        transfer_rpc("status", json!({}), current.clone()),
        transfer_rpc("acquire", json!({}), current),
        head_request(4),
        transfer_rpc(
            "prepare",
            json!({"p_offset":0,"p_size":4,"p_sha256":crate::uploads::sha(b"data")}),
            json!({"state":"active","offset":4,"chunk":{"committed":true}}),
        ),
        transfer_rpc("release", json!({}), json!({})),
    ]);
    let result = crate::uploads::call(&b, "manage_meeting_attachment_upload", &args)
        .await
        .unwrap();
    assert_eq!(result["status"], "unchanged");
    assert_mock_done(&state);
    task.abort();
}
#[tokio::test]
async fn tus_refuses_untrusted_location_and_wrong_object() {
    for wrong_location in [true, false] {
        let (b, state, task) = upload_harness().await;
        let loc = if wrong_location {
            "https://evil.example/storage/v1/upload/resumable/transfer1".into()
        } else {
            b.base
                .join("/storage/v1/upload/resumable/transfer1")
                .unwrap()
                .to_string()
        };
        let current = json!({"state":"active","offset":0,"lease":"lease1","location":loc});
        let mut queue = vec![
            session_read(&upload_session()),
            transfer_rpc("status", json!({}), current.clone()),
            transfer_rpc("acquire", json!({}), current),
        ];
        if !wrong_location {
            let mut head = head_request(0);
            head["headers"]["upload-length"] = json!("5");
            queue.push(head);
        }
        queue.push(transfer_rpc("release", json!({}), json!({})));
        state.queue.lock().unwrap().extend(queue);
        assert!(crate::uploads::call(&b,"manage_meeting_attachment_upload",&json!({"action":"transfer_chunk","id":ID,"offset":0,"data_base64":STANDARD.encode("data")})).await.is_err());
        assert_mock_done(&state);
        task.abort();
    }
}
#[tokio::test]
async fn finalize_verifies_checksum_before_metadata_write() {
    let (b, state, task) = upload_harness().await;
    let mut session = upload_session();
    session["sha256"] = json!("a".repeat(64));
    state.queue.lock().unwrap().extend([session_read(&session),json!({"method":"GET","path":format!("/storage/v1/object/meeting-attachments/{ID}/{ID}.txt"),"query":{},"body":null,"response_base64":STANDARD.encode("data")})]);
    let error = crate::uploads::call(&b, "finalize_meeting_attachment_upload", &json!({"id":ID}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("SHA-256 mismatch"));
    assert_mock_done(&state);
    task.abort();
}

fn rpc_request(name: &str, body: Value, response: Value) -> Value {
    let mut r = json!({"method":"POST","path":format!("/rest/v1/rpc/{name}"),"query":{}});
    r["body"] = body;
    r["response"] = response;
    r
}
#[tokio::test]
async fn optional_external_integrations_keep_auth_boundaries() {
    let (b, state, task) = upload_harness().await;
    let mut b = b;
    b.kb = Some(b.base.clone());
    b.georgia = Some(b.base.join("/georgia-reader").unwrap());
    let admin = rpc_request("is_admin", json!({}), json!(true));
    state.queue.lock().unwrap().extend([json!({"method":"GET","path":"/search","query":{"q":"meeting"},"body":null,"response":{"passages":[]},"no_auth":true}),admin.clone(),json!({"method":"POST","path":"/publish","query":{},"body":{"meeting_id":ID,"attachment_id":ID,"title":"Minutes","source_url":"https://example.org/minutes","document_kind":"minutes","text_origin":"official","confirm_public":true},"response":{"status":"published"},"no_apikey":true}),admin,json!({"method":"POST","path":format!("/documents/{ID}/withdraw"),"query":{},"body":{},"response":{"status":"withdrawn"},"no_apikey":true}),json!({"method":"GET","path":"/georgia-reader","query":{"resource":"sessions","limit":"25","offset":"0"},"body":null,"response":{"source_system":"legis.ga.gov","source_url":"https://www.legis.ga.gov/api/sessions","total":0,"rows":[]},"no_auth":true})]);
    assert_eq!(
        handlers::call(&b, "search_document_contents", json!({"query":" meeting "}))
            .await
            .unwrap(),
        json!({"passages":[]})
    );
    assert_eq!(handlers::call(&b,"publish_meeting_document",json!({"meeting_id":ID,"attachment_id":ID,"title":"Minutes","source_url":"https://example.org/minutes","document_kind":"minutes","text_origin":"official","confirm_public":true})).await.unwrap()["status"],"published");
    assert_eq!(
        handlers::call(&b, "withdraw_public_document", json!({"id":ID}))
            .await
            .unwrap()["status"],
        "withdrawn"
    );
    assert_eq!(
        handlers::call(
            &b,
            "get_georgia_legislative_source",
            json!({"resource":"sessions"})
        )
        .await
        .unwrap()["total"],
        0
    );
    assert_mock_done(&state);
    task.abort();
}
#[tokio::test]
async fn upload_begin_and_finalize_preserve_storage_contract() {
    let (b, state, task) = upload_harness().await;
    let mut s = upload_session();
    s["sha256"] = json!(crate::uploads::sha(b"data"));
    let admin = rpc_request("is_admin", json!({}), json!(true));
    state.queue.lock().unwrap().extend([admin.clone(),rpc_request("begin_meeting_upload",json!({"p_meeting_id":ID,"p_provider":"test","p_source_key":"source","p_file_name":"original.txt","p_mime_type":"text/plain","p_size_bytes":4,"p_sha256":s["sha256"],"p_source_url":"https://example.org/original"}),s.clone()),json!({"method":"POST","path":format!("/storage/v1/object/upload/sign/meeting-attachments/{ID}/{ID}.txt"),"query":{},"body":{},"response":{"url":"/object/upload/sign/meeting-attachments/original.txt?token=upload-test-capability"}}),admin,session_read(&s),json!({"method":"GET","path":format!("/storage/v1/object/meeting-attachments/{ID}/{ID}.txt"),"query":{},"body":null,"response_base64":STANDARD.encode("data")}),rpc_request("finish_meeting_upload",json!({"p_id":ID,"p_verified_sha256":s["sha256"]}),json!({"id":ID,"state":"complete"}))]);
    let result=handlers::call(&b,"begin_meeting_attachment_upload",json!({"meeting_id":ID,"provider":"test","source_key":"source","file_name":"original.txt","size_bytes":4,"sha256":s["sha256"],"source_url":"https://example.org/original"})).await.unwrap();
    assert_eq!(result["headers"]["x-signature"], "upload-test-capability");
    assert_eq!(result["mcp_transfer"]["max_chunk_bytes"], 262_144);
    assert_eq!(
        handlers::call(&b, "finalize_meeting_attachment_upload", json!({"id":ID}))
            .await
            .unwrap()["state"],
        "complete"
    );
    assert_mock_done(&state);
    task.abort();
}
#[tokio::test]
async fn meeting_download_checks_visibility_before_signing() {
    let (b, state, task) = upload_harness().await;
    state.queue.lock().unwrap().extend([rpc_request("get_meeting",json!({"p_id":ID}),json!({"status":"missing"})),rpc_request("get_meeting",json!({"p_id":ID}),json!({"status":"ok","attachments":[{"id":ID,"storage_path":"original.txt","file_name":"Minutes.txt"}]})),json!({"method":"POST","path":"/storage/v1/object/sign/meeting-attachments/original.txt","query":{},"body":{"expiresIn":60},"response":{"signedURL":"/object/sign/meeting-attachments/original.txt?token=test-capability"}})]);
    let args = json!({"meeting_id":ID,"attachment_id":ID});
    assert_eq!(
        handlers::call(&b, "get_meeting_attachment_url", args.clone())
            .await
            .unwrap(),
        json!({"status":"missing"})
    );
    let result = handlers::call(&b, "get_meeting_attachment_url", args)
        .await
        .unwrap();
    assert_eq!(result["expires_in"], 60);
    assert!(
        result["url"]
            .as_str()
            .unwrap()
            .ends_with("token=test-capability&download=Minutes.txt")
    );
    assert_mock_done(&state);
    task.abort();
}
#[tokio::test]
async fn image_and_election_evidence_uploads_verify_originals() {
    let (b, state, task) = upload_harness().await;
    let admin = rpc_request("is_admin", json!({}), json!(true));
    let pdf = b"%PDF-test";
    let hash = crate::uploads::sha(pdf);
    let path = format!("{hash}.pdf");
    state.queue.lock().unwrap().extend([admin.clone(),json!({"method":"POST","path":"/storage/v1/object/org-logos/example.svg","query":{},"body_base64":STANDARD.encode("<svg/>"),"response":{"Key":"org-logos/example.svg"}}),admin.clone(),json!({"method":"POST","path":format!("/storage/v1/object/election-evidence/{path}"),"query":{},"body_base64":STANDARD.encode(pdf),"response":{"Key":path}}),admin,json!({"method":"POST","path":format!("/storage/v1/object/sign/election-evidence/{path}"),"query":{},"body":{"expiresIn":600},"response":{"signedURL":format!("/object/sign/election-evidence/{path}?token=test-capability")}})]);
    assert_eq!(handlers::call(&b,"upload_image",json!({"bucket":"org-logos","path":"example.svg","content_type":"image/svg+xml","data_base64":STANDARD.encode("<svg/>")})).await.unwrap()["path"],"example.svg");
    let result=handlers::call(&b,"manage_election_results",json!({"operation":{"action":"upload","base64":STANDARD.encode(pdf),"mime_type":"application/pdf"}})).await.unwrap();
    assert_eq!(result["sha256"], hash);
    assert_eq!(result["duplicate"], false);
    assert!(
        handlers::call(
            &b,
            "manage_election_results",
            json!({"operation":{"action":"view","path":path}})
        )
        .await
        .unwrap()["signedUrl"]
            .as_str()
            .unwrap()
            .contains("test-capability")
    );
    assert_mock_done(&state);
    task.abort();
}
#[tokio::test]
async fn inline_upload_retains_original_after_uncertain_metadata_write() {
    // The ID/path are random. A route that records the write supplies the same ID to recovery.
    #[derive(Default)]
    struct Record {
        writes: Mutex<Vec<String>>,
    }
    async fn route(State(state): State<Arc<Record>>, request: Request) -> axum::response::Response {
        let path = request.uri().path().to_owned();
        let method = request.method().clone();
        state
            .writes
            .lock()
            .unwrap()
            .push(format!("{method} {path}"));
        if path.ends_with("/is_admin") {
            return axum::Json(json!(true)).into_response();
        }
        if path.ends_with("/get_meeting") {
            return axum::Json(json!({"status":"ok"})).into_response();
        }
        if path.starts_with("/storage/v1/object/meeting-attachments/") {
            return axum::Json(json!({"Key":"stored"})).into_response();
        }
        if method == axum::http::Method::GET {
            return axum::Json(json!([])).into_response();
        }
        (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(json!({"message":"Response uncertain"})),
        )
            .into_response()
    }
    let state = Arc::new(Record::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new().fallback(route).with_state(state.clone());
    let task = tokio::spawn(axum::serve(listener, app).into_future());
    let b = Backend {
        client: reqwest::Client::new(),
        base: format!("http://{address}").parse().unwrap(),
        key: "test-public-key".into(),
        token: "caller-token".into(),
        user: json!({"id":ID}),
        kb: None,
        georgia: None,
    };
    let error = handlers::call(
        &b,
        "upload_meeting_attachment",
        json!({"meeting_id":ID,"file_name":"original.txt","data_base64":STANDARD.encode("data")}),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("retained object"));
    let writes = state.writes.lock().unwrap();
    assert_eq!(writes.len(), 5);
    assert!(writes.iter().all(|r| !r.starts_with("DELETE")));
    task.abort();
}

#[test]
fn simbli_parser_matches_versioned_adapter() {
    let fixtures: Vec<Value> = serde_json::from_str(include_str!("../simbli.json")).unwrap();
    for f in fixtures {
        let result = crate::simbli::parse(f["input"].as_str().unwrap());
        if f["error"] == true {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap(), f["result"], "{}", f["input"]);
        }
    }
}
