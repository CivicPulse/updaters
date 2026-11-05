use crate::{
    backend::Backend,
    handlers::{fields, text},
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::Method;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;
const BUCKET: &str = "meeting-attachments";
const MAX: usize = 26_214_400;
const CHUNK: usize = 262_144;
pub fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn decode(encoded: &str, max: usize) -> Result<Vec<u8>> {
    if encoded.len() > 4 * max.div_ceil(3) || encoded.is_empty() {
        bail!("Invalid or oversized base64");
    }
    let bytes = STANDARD.decode(encoded).context("Invalid base64")?;
    if bytes.is_empty() || bytes.len() > max || STANDARD.encode(&bytes) != encoded {
        bail!("Invalid or oversized base64");
    }
    Ok(bytes)
}
fn document_type(name: &str) -> Result<(&str, &str)> {
    if name.is_empty()
        || name.chars().count() > 255
        || name.trim() != name
        || name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
    {
        bail!("Use a file name of 1–255 characters without paths or control characters");
    }
    let ext = name.rsplit('.').next().unwrap_or("");
    let mime = match ext.to_ascii_lowercase().as_str() {
        "json" => "application/json",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "csv" => "text/csv",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        _ => bail!("Unsupported document type. Use PDF, Office, OpenDocument, TXT or CSV"),
    };
    Ok((ext, mime))
}
fn expired(s: &Value) -> bool {
    chrono::DateTime::parse_from_rfc3339(text(s, "expires_at"))
        .map_or(true, |t| t <= chrono::Utc::now())
}
async fn session(b: &Backend, id: &str) -> Result<Value> {
    b.table(
        "meeting_upload_sessions",
        Method::GET,
        None,
        &[("select", "*".into()), ("id", format!("eq.{id}"))],
        true,
        false,
    )
    .await
}
fn verify(bytes: &[u8], s: &Value) -> Result<()> {
    if bytes.len() != usize::try_from(s["size_bytes"].as_u64().unwrap_or(0))?
        || bytes.is_empty()
        || bytes.len() > MAX
    {
        bail!("Original size mismatch");
    }
    let mime = text(s, "mime_type");
    if (mime == "application/pdf" && !bytes.starts_with(b"%PDF-"))
        || ((mime.contains("openxmlformats") || mime.contains("opendocument"))
            && !bytes.starts_with(b"PK"))
        || ([
            "application/msword",
            "application/vnd.ms-excel",
            "application/vnd.ms-powerpoint",
        ]
        .contains(&mime)
            && !bytes.starts_with(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]))
    {
        bail!("Invalid document signature");
    }
    if mime == "application/json" {
        serde_json::from_slice::<Value>(bytes).context("Invalid JSON original")?;
    }
    if sha(bytes) != text(s, "sha256") {
        bail!("Original SHA-256 mismatch");
    }
    Ok(())
}
#[allow(clippy::too_many_lines)] // Each arm implements one existing upload tool.
pub async fn call(b: &Backend, name: &str, a: &Value) -> Result<Value> {
    match name {
        "upload_meeting_attachment" => {
            let (ext, mime) = document_type(text(a, "file_name"))?;
            let bytes = decode(text(a, "data_base64"), 3 * 1024 * 1024)?;
            let meeting = b
                .rpc("get_meeting", json!({"p_id":a["meeting_id"]}))
                .await?;
            if meeting["status"] != "ok" {
                bail!("Meeting not found or unavailable; mark the event is_meeting=true first");
            }
            let id = uuid::Uuid::new_v4().to_string();
            let path = format!(
                "{}/{id}.{}",
                text(a, "meeting_id"),
                ext.to_ascii_lowercase()
            );
            let len = bytes.len();
            b.upload(BUCKET, &path, bytes, mime, false).await?;
            let columns = "id,meeting_id,file_name,mime_type,size_bytes,created_at";
            let data = json!({"id":id,"meeting_id":a["meeting_id"],"file_name":a["file_name"],"storage_path":path,"mime_type":mime,"size_bytes":len});
            if let Ok(v) = b
                .table(
                    "meeting_attachments",
                    Method::POST,
                    Some(&data),
                    &[("select", columns.into())],
                    true,
                    false,
                )
                .await
            {
                Ok(v)
            } else {
                if let Ok(v) = b
                    .table(
                        "meeting_attachments",
                        Method::GET,
                        None,
                        &[("select", columns.into()), ("id", format!("eq.{id}"))],
                        false,
                        false,
                    )
                    .await
                {
                    if let Some(row) = v.as_array().and_then(|r| r.first()) {
                        return Ok(row.clone());
                    }
                }
                bail!(
                    "Upload outcome uncertain for attachment {id}; retained object {path}. Inspect get_meeting before retrying or reconciling the upload"
                );
            }
        }
        "get_meeting_attachment_url" => {
            let meeting = b
                .rpc("get_meeting", json!({"p_id":a["meeting_id"]}))
                .await?;
            let attachment = if meeting["status"] == "ok" {
                meeting["attachments"]
                    .as_array()
                    .and_then(|rows| rows.iter().find(|r| r["id"] == a["attachment_id"]))
            } else {
                None
            };
            let Some(attachment) = attachment else {
                return Ok(json!({"status":"missing"}));
            };
            Ok(
                json!({"status":"ok","attachment_id":a["attachment_id"],"file_name":attachment["file_name"],"url":b.signed_url(BUCKET,text(attachment,"storage_path"),60,Some(text(attachment,"file_name"))).await?,"expires_in":60}),
            )
        }
        "begin_meeting_attachment_upload" => {
            let (_, mime) = document_type(text(a, "file_name"))?;
            let mut args = fields(
                a,
                &[
                    "meeting_id",
                    "provider",
                    "source_key",
                    "file_name",
                    "size_bytes",
                    "sha256",
                    "source_url",
                ],
            );
            args["p_mime_type"] = json!(mime);
            let mut s = b.rpc("begin_meeting_upload", args).await?;
            if s["state"] != "pending" || expired(&s) {
                s["upload_available"] = json!(false);
                return Ok(s);
            }
            let signed = b
                .storage(
                    Method::POST,
                    &format!(
                        "object/upload/sign/{}",
                        Backend::object_path(BUCKET, text(&s, "storage_path"))
                    ),
                    Some(json!({})),
                )
                .await?;
            let signed_url = signed
                .get("url")
                .or_else(|| signed.get("signedURL"))
                .and_then(Value::as_str)
                .context("Missing signed upload URL")?;
            let signed_url = if signed_url.starts_with("http") {
                Url::parse(signed_url)?
            } else {
                b.base.join(&format!("/storage/v1{signed_url}"))?
            };
            let token = signed_url
                .query_pairs()
                .find(|(k, _)| k == "token")
                .map(|(_, v)| v.into_owned())
                .context("Missing signed upload token")?;
            s["tus_endpoint"] = json!(relay_endpoint(b)?.as_str());
            s["headers"] = json!({"x-signature":token});
            s["chunk_size"] = json!(6 * 1024 * 1024);
            s["mcp_transfer"] = json!({"tool":"manage_meeting_attachment_upload","action":"transfer_chunk","max_chunk_bytes":CHUNK,"status_action":"transfer_status"});
            s["metadata"] = json!({"bucketName":BUCKET,"objectName":s["storage_path"],"contentType":s["mime_type"]});
            s["token_expires_in"] = json!(7200);
            Ok(s)
        }
        "finalize_meeting_attachment_upload" => {
            let s = session(b, text(a, "id")).await?;
            if s["state"] != "complete" {
                if s["state"] != "pending" || expired(&s) {
                    bail!("Upload cancelled or expired");
                }
                let bytes = b.download(BUCKET, text(&s, "storage_path")).await?;
                verify(&bytes, &s)?;
            }
            b.rpc(
                "finish_meeting_upload",
                json!({"p_id":a["id"],"p_verified_sha256":s["sha256"]}),
            )
            .await
        }
        "manage_meeting_attachment_upload" => {
            let action = text(a, "action");
            if action == "list_cleanup" {
                return b
                    .table(
                        "meeting_upload_sessions",
                        Method::GET,
                        None,
                        &[
                            ("select", "*".into()),
                            ("state", "neq.complete".into()),
                            (
                                "or",
                                format!(
                                    "(state.eq.cancelled,expires_at.lt.{})",
                                    chrono::Utc::now()
                                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                                ),
                            ),
                            ("order", "id".into()),
                            ("offset", a["offset"].to_string()),
                            ("limit", "100".into()),
                        ],
                        false,
                        false,
                    )
                    .await;
            }
            let id = a["id"]
                .as_str()
                .context("id is required for session actions")?;
            if action == "cancel" {
                let s = b.rpc("cancel_meeting_upload", json!({"p_id":id})).await?;
                b.remove_object(BUCKET, text(&s, "storage_path"))
                    .await
                    .with_context(|| format!("Session cancelled; retry cleanup for {id}"))?;
                return Ok(s);
            }
            let mut s = session(b, id).await?;
            if action == "transfer_chunk" || action == "transfer_status" {
                return transfer(b, &s, a, action == "transfer_status").await;
            }
            let request = b.request(
                Method::GET,
                &format!(
                    "/storage/v1/object/info/{}",
                    Backend::object_path(BUCKET, text(&s, "storage_path"))
                ),
            )?;
            let response = request.send().await?;
            let status = response.status();
            let object = if status.is_success() {
                response.json().await?
            } else if status.as_u16() == 404 || status.as_u16() == 400 {
                Value::Null
            } else {
                bail!("Storage object metadata unavailable");
            };
            s["expired"] = json!(expired(&s));
            s["stored_object"] = object;
            s["attachment_id"] = if s["state"] == "complete" {
                s["id"].clone()
            } else {
                Value::Null
            };
            Ok(s)
        }
        _ => bail!("Unknown upload tool"),
    }
}
fn relay_endpoint(b: &Backend) -> Result<Url> {
    let mut url = b.base.clone();
    if let Some(host) = url
        .host_str()
        .filter(|h| h.ends_with(".supabase.co") && !h.ends_with(".storage.supabase.co"))
    {
        let h = host.replace(".supabase.co", ".storage.supabase.co");
        url.set_host(Some(&h))?;
    }
    url.set_path("/storage/v1/upload/resumable");
    Ok(url)
}
fn location(value: &str, endpoint: &Url) -> Result<Url> {
    let url = endpoint.join(value)?;
    if url.origin() != endpoint.origin()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.path().starts_with(&format!("{}/", endpoint.path()))
        || url.path() == format!("{}/", endpoint.path())
    {
        bail!("Storage returned an unexpected transfer location");
    }
    Ok(url)
}
async fn state(
    b: &Backend,
    s: &Value,
    action: &str,
    lease: Option<&Value>,
    extra: Value,
) -> Result<Value> {
    let mut args = json!({"p_action":action,"p_id":s["id"]});
    if let Some(lease) = lease {
        args["p_lease"] = lease.clone();
    }
    if let Some(extra) = extra.as_object() {
        for (k, v) in extra {
            args[k] = v.clone();
        }
    }
    b.rpc("manage_meeting_upload_transfer", args).await
}
fn public_state(s: &Value, r: &Value, extra: &Value) -> Value {
    let mut result = json!({"state":r["state"],"offset":r.get("offset").and_then(|v|v.as_u64().or_else(||v.as_str().and_then(|s|s.parse().ok()))).unwrap_or(0),"size_bytes":s["size_bytes"],"max_chunk_bytes":CHUNK});
    if !r["pending_chunk"].is_null() {
        result["pending_chunk"] = json!({"offset":r["pending_chunk"]["chunk_offset"],"size_bytes":r["pending_chunk"]["size_bytes"]});
    }
    for (k, v) in extra.as_object().expect("extra object") {
        result[k] = v.clone();
    }
    result
}
fn tus(b: &Backend, method: Method, url: Url) -> reqwest::RequestBuilder {
    b.client
        .request(method, url)
        .bearer_auth(&b.token)
        .header("apikey", &b.key)
        .header("Tus-Resumable", "1.0.0")
        .timeout(std::time::Duration::from_secs(20))
}
async fn head(b: &Backend, s: &Value, url: &Url) -> Result<u64> {
    let response = tus(b, Method::HEAD, url.clone()).send().await.context(
        "Storage HEAD receipt unavailable; retry transfer_status before resending bytes",
    )?;
    if !response.status().is_success() {
        bail!("Storage HEAD unavailable; retain the session and reconcile");
    }
    let headers = response.headers();
    let size = headers
        .get("upload-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .context("Missing Upload-Length")?;
    let offset = headers
        .get("upload-offset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .context("Missing Upload-Offset")?;
    let mut metadata = std::collections::HashMap::new();
    for item in headers
        .get("upload-metadata")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(',')
        .filter(|s| !s.is_empty())
    {
        let (k, v) = item.trim().split_once(' ').unwrap_or((item, ""));
        metadata.insert(k, String::from_utf8(STANDARD.decode(v)?)?);
    }
    if size != number(&s["size_bytes"])
        || offset > size
        || metadata.get("bucketName").map(String::as_str) != Some(BUCKET)
        || metadata.get("objectName").map(String::as_str) != Some(text(s, "storage_path"))
        || metadata.get("contentType").map(String::as_str) != Some(text(s, "mime_type"))
    {
        bail!("Storage transfer does not match this session; no bytes forwarded");
    }
    Ok(offset)
}
fn number(v: &Value) -> u64 {
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0)
}
async fn transfer(b: &Backend, s: &Value, a: &Value, status_only: bool) -> Result<Value> {
    let endpoint = relay_endpoint(b)?;
    let bytes = if status_only {
        None
    } else {
        Some(decode(text(a, "data_base64"), CHUNK)?)
    };
    let offset = number(&a["offset"]);
    if offset
        .checked_add(bytes.as_ref().map_or(0, |v| v.len() as u64))
        .is_none_or(|end| end > number(&s["size_bytes"]))
    {
        bail!("Chunk offset/size outside original document");
    }
    let current = state(b, s, "status", None, json!({})).await?;
    if s["state"] == "complete" {
        return Ok(public_state(
            s,
            &current,
            &json!({"state":"complete","offset":s["size_bytes"],"attachment_id":s["id"]}),
        ));
    }
    if status_only && current["state"] == "not_started" {
        return Ok(public_state(s, &current, &json!({})));
    }
    if s["state"] != "pending" || expired(s) {
        bail!("Upload cancelled or expired");
    }
    let current = state(b, s, "acquire", None, json!({})).await?;
    let lease = current["lease"].clone();
    let result = transfer_leased(
        b,
        s,
        a,
        status_only,
        bytes,
        offset,
        &endpoint,
        current,
        &lease,
    )
    .await;
    // A failed release is safe: the durable lease expires. Preserve the transfer result.
    let _ = state(b, s, "release", Some(&lease), json!({})).await;
    result
}
#[allow(clippy::too_many_lines, clippy::too_many_arguments)] // Ordered durable TUS state machine.
async fn transfer_leased(
    b: &Backend,
    s: &Value,
    _a: &Value,
    status_only: bool,
    bytes: Option<Vec<u8>>,
    offset: u64,
    endpoint: &Url,
    mut current: Value,
    lease: &Value,
) -> Result<Value> {
    if current["location"].as_str().is_none_or(str::is_empty) {
        if current["creating"] == true {
            return Ok(public_state(
                s,
                &current,
                &json!({"state":"creation_uncertain","message":"A previous Storage creation has no saved Location. Do not repeat POST or invent a source key; operator reconciliation is required."}),
            ));
        }
        if status_only {
            return Ok(public_state(s, &current, &json!({"state":"not_started"})));
        }
        if offset != 0 {
            bail!("Start a new transfer at offset 0");
        }
        state(b, s, "creating", Some(lease), json!({})).await?;
        let metadata = format!(
            "bucketName {},objectName {},contentType {}",
            STANDARD.encode(BUCKET),
            STANDARD.encode(text(s, "storage_path")),
            STANDARD.encode(text(s, "mime_type"))
        );
        let created = tus(b, Method::POST, endpoint.clone())
            .header("Upload-Length", number(&s["size_bytes"]))
            .header("Upload-Metadata", metadata)
            .send()
            .await;
        let Ok(created) = created else {
            return Ok(public_state(
                s,
                &current,
                &json!({"state":"creation_uncertain","message":"Storage creation receipt unavailable; do not blindly create another transfer."}),
            ));
        };
        let status = created.status().as_u16();
        let loc = created
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if [400, 403].contains(&status) && loc.is_none() {
            let rejection = created.json::<Value>().await.unwrap_or(Value::Null);
            if rejection["code"] == "AccessDenied"
                && rejection["error"] == "Unauthorized"
                && (rejection["statusCode"] == "403" || rejection["statusCode"] == 403)
                && rejection["message"] == "\"exp\" claim timestamp check failed"
            {
                return match state(b, s, "creation_auth_rejected", Some(lease), json!({})).await {
                    Ok(r) => Ok(public_state(
                        s,
                        &r,
                        &json!({"state":"authentication_required","message":"Storage rejected expired authentication before creation. Refresh your connection and retry this same session and chunk."}),
                    )),
                    Err(_) => Ok(public_state(
                        s,
                        &current,
                        &json!({"state":"creation_uncertain","message":"Storage rejected expired authentication, but its durable creation intent could not be cleared; operator reconciliation required."}),
                    )),
                };
            }
            return Ok(public_state(
                s,
                &current,
                &json!({"state":"creation_uncertain","message":format!("Storage creation not confirmed ({status}); operator reconciliation required.")}),
            ));
        }
        if status != 201 || loc.is_none() {
            return Ok(public_state(
                s,
                &current,
                &json!({"state":"creation_uncertain","message":format!("Storage creation not confirmed ({status}); operator reconciliation required.")}),
            ));
        }
        let loc = location(loc.as_deref().unwrap_or(""), endpoint)?;
        current = state(
            b,
            s,
            "location",
            Some(lease),
            json!({"p_location":loc.as_str()}),
        )
        .await?;
    }
    let loc = location(text(&current, "location"), endpoint)?;
    let mut stored = head(b, s, &loc).await?;
    if status_only {
        let pending = current["pending_chunk"].clone();
        if !pending.is_null() {
            let begin = number(&pending["chunk_offset"]);
            let end = begin + number(&pending["size_bytes"]);
            if stored == end {
                current = state(b, s, "commit", Some(lease), json!({"p_offset":begin})).await?;
            }
            if stored < begin || stored > end {
                bail!("Storage offset outside pending receipt; no bytes forwarded");
            }
        } else if stored != number(&current["offset"]) {
            bail!("Storage offset differs from committed receipts; no bytes forwarded");
        }
        return Ok(public_state(s, &current, &json!({"storage_offset":stored})));
    }
    let bytes = bytes.context("Missing chunk")?;
    current = state(
        b,
        s,
        "prepare",
        Some(lease),
        json!({"p_offset":offset,"p_size":bytes.len(),"p_sha256":sha(&bytes)}),
    )
    .await?;
    if current["chunk"]["committed"] == true {
        if stored < number(&current["offset"]) {
            bail!("Storage offset behind committed receipts; no bytes forwarded");
        }
        return Ok(public_state(
            s,
            &current,
            &json!({"status":"unchanged","storage_offset":stored}),
        ));
    }
    let end = offset + bytes.len() as u64;
    if stored < offset || stored > end {
        bail!("Storage offset conflicts with pending chunk; no bytes forwarded");
    }
    if stored < end {
        let result = tus(b, Method::PATCH, loc.clone())
            .header("Upload-Offset", stored)
            .header("Content-Type", "application/offset+octet-stream")
            .body(bytes[usize::try_from(stored - offset)?..].to_vec())
            .send()
            .await;
        let failure = result.map_or(true, |r| !r.status().is_success());
        stored = head(b, s, &loc).await?;
        if stored != end {
            return Ok(public_state(
                s,
                &current,
                &json!({"status":"pending","storage_offset":stored,"message":"Retry the exact pending chunk; HEAD will reconcile its accepted prefix."}),
            ));
        }
        current = state(b, s, "commit", Some(lease), json!({"p_offset":offset})).await?;
        return Ok(public_state(
            s,
            &current,
            &json!({"status":if failure{"reconciled"}else{"accepted"},"storage_offset":stored}),
        ));
    }
    current = state(b, s, "commit", Some(lease), json!({"p_offset":offset})).await?;
    Ok(public_state(
        s,
        &current,
        &json!({"status":"reconciled","storage_offset":stored}),
    ))
}
