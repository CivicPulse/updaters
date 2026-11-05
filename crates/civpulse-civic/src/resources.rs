//! Explicit civic table allowlist; RLS and existing workflow RPCs remain authoritative.
use crate::{
    backend::{Backend, BackendError},
    catalog, handlers,
};
use anyhow::{Context, Result, ensure};
use reqwest::Method;
use serde_json::{Value, json};
use std::sync::OnceLock;

/// Reviewed schema inventory, excluding internal worker state.
///
/// # Panics
/// Only if the checked-in inventory is invalid.
pub fn manifest() -> &'static Value {
    static DATA: OnceLock<Value> = OnceLock::new();
    DATA.get_or_init(|| {
        serde_json::from_str(include_str!("../reference/resources.json"))
            .expect("embedded resource manifest")
    })
}
fn resource(name: &str) -> Result<&'static Value> {
    manifest()["resources"]
        .as_array()
        .context("Invalid resource manifest")?
        .iter()
        .find(|r| r["name"] == name)
        .context("Unknown civic resource")
}
fn forbidden(message: &str) -> anyhow::Error {
    BackendError {
        status: 403,
        message: message.to_owned(),
    }
    .into()
}
fn column<'a>(r: &'a Value, name: &str) -> Result<&'a Value> {
    r["columns"]
        .as_array()
        .context("Missing columns")?
        .iter()
        .find(|c| c["name"] == name)
        .context("Unknown civic field")
}
fn scalar(value: &Value, c: &Value) -> Result<String> {
    ensure!(
        !value.is_null() && !value.is_object() && !value.is_array(),
        "Key/filter must be a non-null scalar"
    );
    if c["type"] == "uuid" {
        uuid::Uuid::parse_str(value.as_str().context("UUID must be a string")?)?;
    }
    Ok(value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned))
}
fn keys(r: &Value, value: &Value) -> Result<Vec<(String, String)>> {
    let object = value.as_object().context("keys must be an object")?;
    let pk = r["primary_key"].as_array().context("Missing primary key")?;
    ensure!(
        object.len() == pk.len() && !pk.is_empty(),
        "Supply every primary key, and no other keys"
    );
    pk.iter()
        .map(|key| {
            let name = key.as_str().context("Invalid key")?;
            let value = object.get(name).context("Missing primary key")?;
            Ok((
                name.to_owned(),
                format!("eq.{}", scalar(value, column(r, name)?)?),
            ))
        })
        .collect()
}
fn data(r: &Value, value: &Value, create: bool) -> Result<()> {
    let object = value.as_object().context("data must be an object")?;
    ensure!(!object.is_empty(), "Provide at least one field");
    for (name, value) in object {
        let c = column(r, name)?;
        ensure!(
            c["generated"] != true && !["created_at", "updated_at"].contains(&name.as_str()),
            "Generated/system fields cannot be written"
        );
        ensure!(
            create
                || !r["primary_key"]
                    .as_array()
                    .is_some_and(|keys| keys.contains(&json!(name))),
            "Primary keys cannot be changed"
        );
        if value.is_null() {
            continue;
        }
        let kind = c["type"].as_str().unwrap_or("");
        match kind {
            "uuid" => {
                uuid::Uuid::parse_str(value.as_str().context("UUID must be a string")?)?;
            }
            "boolean" => ensure!(value.is_boolean(), "Boolean field required"),
            "integer" | "bigint" | "smallint" => {
                ensure!(value.is_i64() || value.is_u64(), "Integer field required");
            }
            "text" | "date" | "timestamp with time zone" => {
                ensure!(value.is_string(), "String field required");
            }
            _ => {} // PostgreSQL owns arrays, numeric precision, JSON, geometry and domain constraints.
        }
    }
    if create {
        for c in r["columns"].as_array().context("Missing columns")? {
            ensure!(
                c["required"] != true || object.contains_key(c["name"].as_str().unwrap_or("")),
                "Missing required civic field"
            );
        }
    }
    Ok(())
}

/// Dispatch only reviewed civic resources, never arbitrary tables or SQL.
///
/// # Errors
/// Rejects invalid keys/fields, unsupported direct writes and backend permission failures.
#[allow(clippy::too_many_lines, clippy::many_single_char_names)] // One explicit CRUD dispatch with shared validation.
pub async fn call(b: &Backend, operation: &str, a: &Value) -> Result<Value> {
    if operation == "list_civic_resources" {
        return Ok(manifest().clone());
    }
    let name = handlers::text(a, "resource");
    let r = resource(name)?;
    let mode = match operation {
        "search_civic_data" | "get_civic_data" => "read",
        "create_civic_data" => "create",
        "update_civic_data" => "update",
        "delete_civic_data" => "delete",
        _ => anyhow::bail!("Unknown civic operation"),
    };
    // Core discovery must retain listed/visibility gates implemented by the existing RPC.
    if operation == "search_civic_data"
        && let Some(kind) = r["record_type"].as_str()
    {
        ensure!(
            a["filters"]
                .as_object()
                .is_some_and(serde_json::Map::is_empty),
            "Use search_site for core record filters"
        );
        return Box::pin(handlers::call(
            b,
            "search_site",
            json!({"type":kind,"query":a["query"],"limit":a["limit"],"offset":a["offset"]}),
        ))
        .await;
    }
    // Core visibility-aware readers intentionally expose projections beyond raw-table RLS.
    if operation == "get_civic_data"
        && let Some(kind) = r["record_type"].as_str()
    {
        keys(r, &a["keys"])?;
        return Box::pin(handlers::call(
            b,
            "get_civic_record",
            json!({"type":kind,"id":a["keys"]["id"]}),
        ))
        .await;
    }
    if r["table_operations"][mode] != true {
        return Err(forbidden(
            "Direct operation is not permitted; use the resource's advertised guarded workflows",
        ));
    }
    if mode == "read" && r["read_requires_admin"] == true {
        b.admin().await?;
    }
    let mut q = Vec::<(String, String)>::new();
    let select = r["columns"]
        .as_array()
        .context("Missing columns")?
        .iter()
        .filter(|c| c["generated"] != true)
        .filter_map(|c| c["name"].as_str())
        .collect::<Vec<_>>()
        .join(",");
    q.push(("select".into(), select));
    if operation == "search_civic_data" {
        let pk = r["primary_key"]
            .as_array()
            .context("Missing primary keys")?;
        q.push((
            "order".into(),
            pk.iter()
                .filter_map(Value::as_str)
                .map(|s| format!("{s}.asc"))
                .collect::<Vec<_>>()
                .join(","),
        ));
        for key in ["limit", "offset"] {
            q.push((key.into(), a[key].to_string()));
        }
        for (name, value) in a["filters"].as_object().context("Invalid filters")? {
            let c = column(r, name)?;
            q.push((
                name.clone(),
                if value.is_null() {
                    "is.null".into()
                } else {
                    format!("eq.{}", scalar(value, c)?)
                },
            ));
        }
        let query = handlers::text(a, "query").trim();
        if !query.is_empty() {
            let pattern = serde_json::to_string(&format!(
                "%{}%",
                query
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
                    .replace('*', "\\*")
            ))?;
            let expressions = r["columns"]
                .as_array()
                .context("Missing columns")?
                .iter()
                .filter(|c| c["type"] == "text")
                .filter_map(|c| c["name"].as_str())
                .map(|name| format!("{name}.ilike.{pattern}"))
                .collect::<Vec<_>>();
            ensure!(
                !expressions.is_empty(),
                "Resource has no text search fields; use exact filters"
            );
            q.push(("or".into(), format!("({})", expressions.join(","))));
        }
    } else if operation != "create_civic_data" {
        q.extend(keys(r, &a["keys"])?);
    }
    let method = match mode {
        "create" => Method::POST,
        "update" => Method::PATCH,
        "delete" => Method::DELETE,
        _ => Method::GET,
    };
    if mode == "create" || mode == "update" {
        data(r, &a["data"], mode == "create")?;
    }
    let mut request = b
        .request(method, &format!("/rest/v1/{name}"))?
        .query(&q)
        .header("Prefer", "return=representation");
    if mode == "create" || mode == "update" {
        request = request.json(&a["data"]);
    }
    let rows = Backend::send(request).await?;
    if !rows.is_array() {
        return Err(BackendError {
            status: 502,
            message: "Invalid civic table response".into(),
        }
        .into());
    }
    if operation != "search_civic_data" && rows.as_array().is_some_and(Vec::is_empty) {
        return Err(BackendError {
            status: 404,
            message: "Record unavailable or permission denied".into(),
        }
        .into());
    }
    Ok(json!({"resource":name,"data":rows,"limit":a.get("limit"),"offset":a.get("offset")}))
}

/// Validate and invoke a reviewed workflow RPC with its original argument names.
///
/// # Errors
/// Input validation and existing RPC authorization remain mandatory.
#[allow(clippy::too_many_lines)] // Explicit reviewed RPC argument mapping.
pub async fn workflow(b: &Backend, name: &str, a: &Value) -> Result<Value> {
    let (rpc, args) = match name {
        "get_sos_results" => (
            name,
            handlers::fields(
                a,
                &[
                    "election_id",
                    "snapshot_id",
                    "jurisdiction_index",
                    "offset",
                    "limit",
                ],
            ),
        ),
        "get_sos_results_mappings" => (name, handlers::fields(a, &["election_id"])),
        "put_sos_results_mapping" => {
            ensure!(
                a["contest_id"].is_null() != a["question_id"].is_null(),
                "Exactly one contest_id or question_id is required"
            );
            (
                name,
                handlers::fields(
                    a,
                    &[
                        "election_id",
                        "jurisdiction_id",
                        "source_contest_id",
                        "contest_id",
                        "question_id",
                        "choices",
                    ],
                ),
            )
        }
        "delete_sos_results_mapping" => (
            name,
            handlers::fields(a, &["election_id", "jurisdiction_id", "source_contest_id"]),
        ),
        "begin_sos_results" => (
            name,
            handlers::fields(
                a,
                &[
                    "election_id",
                    "source_url",
                    "sha256",
                    "header",
                    "jurisdictions",
                ],
            ),
        ),
        "stage_sos_results" => {
            ensure!(
                serde_json::to_vec(&a["items"])?.len() <= 4 * 1024 * 1024,
                "Stage batch exceeds 4 MiB"
            );
            (name, handlers::fields(a, &["snapshot_id", "items"]))
        }
        "publish_sos_results" => (
            name,
            handlers::fields(a, &["snapshot_id", "expected_snapshot_id"]),
        ),
        "save_records_request" => (name, json!({"p_id":a["id"],"p_values":a["values"]})),
        "add_records_request_item" => (
            name,
            json!({"p_id":a["id"],"p_kind":a["kind"],"p_values":a["values"]}),
        ),
        "remove_records_request_item" => (name, handlers::fields(a, &["id", "kind", "item_id"])),
        "transfer_records_request" => (name, handlers::fields(a, &["id", "to_user_id", "reason"])),
        "search_records_requests" => (
            name,
            handlers::fields(
                a,
                &["query", "stage", "organization_id", "managed", "offset"],
            ),
        ),
        "get_records_request" => (name, handlers::fields(a, &["id"])),
        "get_object_history" => (
            name,
            handlers::fields(a, &["table", "object_id", "before", "limit"]),
        ),
        "save_short_link" => (
            name,
            handlers::fields(a, &["id", "title", "target_url", "active"]),
        ),
        "get_voter_roll_history" => {
            b.admin().await?;
            (
                name,
                handlers::fields(a, &["registration_number", "limit", "offset"]),
            )
        }
        "begin_voter_roll_import" => (
            name,
            handlers::fields(
                a,
                &[
                    "county_fips",
                    "snapshot_date",
                    "source_filename",
                    "source_sha512",
                    "expected_records",
                    "supersedes_id",
                ],
            ),
        ),
        "append_voter_roll_records" => {
            ensure!(
                serde_json::to_vec(&a["rows"])?.len() <= 2 * 1024 * 1024,
                "Voter row batch exceeds 2 MiB"
            );
            (name, handlers::fields(a, &["import_id", "rows"]))
        }
        "complete_voter_roll_import" => (name, handlers::fields(a, &["import_id"])),
        _ => anyhow::bail!("Unknown civic workflow"),
    };
    // The catalog itself is checked separately by both transports.
    let _ = catalog::tools();
    b.rpc(rpc, args).await
}

pub(crate) async fn file(backend: &Backend, args: &Value) -> Result<Value> {
    use base64::Engine;
    let request_id = handlers::text(args, "request_id");
    let request = backend
        .rpc("get_records_request", json!({"p_id":request_id}))
        .await?;
    ensure!(request["status"] == "ok", "Request unavailable");
    let action = handlers::text(args, "action");
    if action == "upload" {
        if request["can_manage"] != true {
            return Err(forbidden("Request owner or administrator required"));
        }
        let path = handlers::text(args, "storage_path");
        let (prefix, filename) = path.split_once('/').context("storage_path required")?;
        let (object_id, _) = filename
            .split_once('.')
            .context("File UUID and extension required")?;
        ensure!(
            prefix == request_id,
            "File path must belong to this request"
        );
        uuid::Uuid::parse_str(object_id)?;
        let name = handlers::text(args, "file_name");
        let mime = handlers::text(args, "mime_type");
        ensure!(
            !name.is_empty() && !mime.is_empty(),
            "file_name and mime_type required"
        );
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(handlers::text(args, "data_base64"))?;
        ensure!(
            !bytes.is_empty() && bytes.len() <= 3 * 1024 * 1024,
            "Original must contain 1 byte to 3 MiB"
        );
        let size = bytes.len();
        backend
            .upload("records-request-files", path, bytes, mime, false)
            .await?;
        let id=backend.rpc("add_records_request_item",json!({"p_id":request_id,"p_kind":"file","p_values":{
            "file_name":name,"storage_path":path,"mime_type":mime,"size_bytes":size,"is_private":args["is_private"]}})).await
            .with_context(||format!("Private original retained at {path}; reconcile using add_records_request_item"))?;
        return Ok(json!({"id":id,"storage_path":path}));
    }
    let attachment = request["files"]
        .as_array()
        .context("Files unavailable")?
        .iter()
        .find(|f| f["id"] == args["file_id"])
        .context("File unavailable")?;
    let path = attachment["storage_path"]
        .as_str()
        .context("File path unavailable")?;
    if action == "url" {
        return Ok(
            json!({"url":backend.signed_url("records-request-files",path,300,None).await?,"expires_in":300}),
        );
    }
    if request["can_manage"] != true {
        return Err(forbidden("Request owner or administrator required"));
    }
    backend
        .storage(
            Method::DELETE,
            "object/records-request-files",
            Some(json!({"prefixes":[path]})),
        )
        .await?;
    backend
        .rpc(
            "remove_records_request_item",
            json!({"p_id":request_id,"p_kind":"file","p_item_id":args["file_id"]}),
        )
        .await?;
    Ok(json!({"id":args["file_id"],"status":"deleted"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_live_table_has_a_reviewed_disposition_and_workflows_exist() {
        let rows = manifest()["resources"].as_array().unwrap();
        assert_eq!(
            rows.len() + manifest()["excluded"].as_object().unwrap().len(),
            81
        );
        let mut names = std::collections::HashSet::new();
        for row in rows {
            assert!(names.insert(row["name"].as_str().unwrap()));
            assert_eq!(row["rls"], true);
            assert!(
                row["table_operations"]
                    .as_object()
                    .unwrap()
                    .values()
                    .any(|value| *value == true)
                    || row["workflows"].as_array().is_some_and(|v| !v.is_empty()),
                "Resource has no supported surface"
            );
            assert_ne!(
                row["primary_key"].as_array().unwrap().as_slice(),
                &[] as &[Value]
            );
            for key in row["primary_key"].as_array().unwrap() {
                assert!(column(row, key.as_str().unwrap()).is_ok());
            }
            for workflow in row["workflows"].as_array().unwrap() {
                assert!(
                    catalog::tools().iter().any(|t| t["name"] == *workflow),
                    "Missing {workflow}"
                );
            }
        }
        for name in [
            "search_civic_data",
            "get_civic_data",
            "create_civic_data",
            "update_civic_data",
            "delete_civic_data",
        ] {
            let tool = catalog::tools().iter().find(|t| t["name"] == name).unwrap();
            assert_eq!(
                tool["inputSchema"]["properties"]["resource"]["enum"]
                    .as_array()
                    .unwrap()
                    .len(),
                rows.len()
            );
            assert_eq!(tool["_meta"]["civpulse/authorization"], "database");
        }
    }
    #[test]
    fn exact_composite_identity_and_patch_fields_are_mandatory() {
        let row = resource("contest_candidates").unwrap();
        assert!(
            keys(
                row,
                &json!({"contest_id":"00000000-0000-4000-8000-000000000001"})
            )
            .is_err()
        );
        assert!(
            keys(
                row,
                &json!({"contest_id":"00000000-0000-4000-8000-000000000001","person_id":"bad"})
            )
            .is_err()
        );
        assert!(keys(row,&json!({"contest_id":"00000000-0000-4000-8000-000000000001","person_id":"00000000-0000-4000-8000-000000000002"})).is_ok());
        let row = resource("people").unwrap();
        for patch in [
            json!({}),
            json!({"id":"00000000-0000-4000-8000-000000000001"}),
            json!({"created_at":"2026-01-01"}),
            json!({"site_search_document":"forged"}),
            json!({"unknown":true}),
        ] {
            assert!(data(row, &patch, false).is_err());
        }
        assert!(data(row, &json!({"full_name":"New name"}), false).is_ok());
        assert!(resource("admins").is_err());
        assert!(resource("../auth/users").is_err());
    }
}
