use crate::{backend::Backend, catalog};
use anyhow::{Context, Result, bail};
use reqwest::Method;
use serde_json::{Value, json};
#[must_use]
pub fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
#[must_use]
pub fn fields(v: &Value, names: &[&str]) -> Value {
    names
        .iter()
        .map(|key| {
            (
                format!("p_{key}"),
                v.get(*key).cloned().unwrap_or(Value::Null),
            )
        })
        .collect()
}
fn without(v: &Value, keys: &[&str]) -> Value {
    v.as_object()
        .expect("validated object")
        .iter()
        .filter(|(k, _)| !keys.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}
#[allow(clippy::too_many_lines)] // Explicit allowlist for the complete V2 catalog.
pub async fn call(b: &Backend, name: &str, a: Value) -> Result<Value> {
    let a = catalog::arguments(name, a)?;
    let tool = catalog::tools()
        .iter()
        .find(|t| t["name"] == name)
        .context("Unknown tool")?;
    // The mixed school tool permits browse; every other mutation requires admin.
    if tool["annotations"]["readOnlyHint"] == false && name != "school_finances" {
        b.admin().await?;
    }
    match name {
        "import_meeting_source" => crate::meeting_source::import(b, &a).await,
        "accept_obligation_report" | "accept_user_fee_report" => {
            b.rpc(
                name,
                fields(&a, &["version", "provider", "report", "rows", "dry_run"]),
            )
            .await
        }
        "accept_officeholder_observations" => {
            b.rpc(
                name,
                fields(&a, &["version", "provider", "rows", "dry_run"]),
            )
            .await
        }
        "read_officeholder_observations" => {
            b.rpc(
                name,
                fields(
                    &a,
                    &[
                        "person_id",
                        "organization_id",
                        "as_of",
                        "history",
                        "offset",
                        "limit",
                    ],
                ),
            )
            .await
        }
        "browse_obligations" => {
            b.rpc(
                name,
                fields(
                    &a,
                    &[
                        "organization_id",
                        "year",
                        "query",
                        "vendor_kind",
                        "vendor_id",
                        "observation_id",
                        "history",
                        "offset",
                        "limit",
                    ],
                ),
            )
            .await
        }
        "browse_user_fees" => {
            b.rpc(
                name,
                fields(
                    &a,
                    &[
                        "organization_id",
                        "year",
                        "fee_id",
                        "query",
                        "history",
                        "offset",
                        "limit",
                    ],
                ),
            )
            .await
        }
        "whoami" => {
            let admin = b.rpc("is_admin", json!({})).await?;
            Ok(json!(
                { "user_id" : b.user["id"], "email" : b.user.get("email").unwrap_or(&
                Value::Null), "is_admin" : admin == true, "role" : if admin == true {
                "admin" } else { "user" } }
            ))
        }
        "search_users" => {
            b.admin().await?;
            b.rpc("admin_list_users", fields(&a, &["query", "offset"]))
                .await
        }
        "promote_user_to_admin" => {
            b.rpc(name, fields(&a, &["user_id"])).await?;
            Ok(json!({ "user_id" : a["user_id"], "role" : "admin" }))
        }
        "search_site" => search(b, &a).await,
        "get_civic_record" => get_record(b, text(&a, "type"), a["id"].clone()).await,
        "get_county_statistic_catalog" => county_catalog(b).await,
        "search_county_statistics" => {
            b.rpc(
                name,
                fields(
                    &a,
                    &[
                        "query",
                        "county_fips",
                        "indicator_code",
                        "topic",
                        "limit",
                        "offset",
                    ],
                ),
            )
            .await
        }
        "save_county_statistic" => {
            let mut row = without(&a, &["expected_updated_at"]);
            for k in [
                "numerator",
                "denominator",
                "margin_of_error",
                "lower_bound",
                "upper_bound",
                "confidence_level",
            ] {
                if row.get(k).is_none() {
                    row[k] = Value::Null;
                }
            }
            let mut q = vec![("select", "*".into())];
            let method = if a["expected_updated_at"].is_null() {
                Method::POST
            } else {
                for k in ["county_fips", "indicator_code"] {
                    q.push((k, format!("eq.{}", text(&a, k))));
                }
                q.push((
                    "updated_at",
                    format!("eq.{}", text(&a, "expected_updated_at")),
                ));
                Method::PATCH
            };
            let data = b
                .table("county_statistics", method, Some(&row), &q, false, false)
                .await?;
            data.as_array()
                .and_then(|r| r.first())
                .cloned()
                .context("Statistic changed or no longer exists. Read it again before saving.")
        }
        "save_county_statistic_source" => {
            b.table(
                "county_statistic_sources",
                Method::POST,
                Some(&a),
                &[("select", "*".into())],
                true,
                true,
            )
            .await
        }
        "school_finances" => {
            let op = &a["operation"];
            if op["action"] == "browse" {
                b.rpc(
                    "browse_school_finances",
                    fields(
                        op,
                        &[
                            "dataset",
                            "year",
                            "query",
                            "offset",
                            "limit",
                            "sort",
                            "person_id",
                        ],
                    ),
                )
                .await
            } else {
                b.admin().await?;
                b.rpc(
                    "manage_school_finances",
                    json!({ "p_action" : op["action"], "p_payload" : op["payload"] }),
                )
                .await
            }
        }
        "save_relationship" | "delete_relationship" => {
            let r = &a["request"];
            let resource = text(r, "relationship");
            if name == "save_relationship" {
                save(b, resource, &r["change"]).await
            } else {
                delete(b, resource, &without(r, &["relationship"])).await
            }
        }
        "read_related_records" => related(b, &a["request"]).await,
        "save_postgis_layer" => {
            let id = b
                .rpc(
                    "mcp_save_postgis_layer",
                    json!(
                        { "p_layer" : without(& a,& ["geojson"]), "p_geojson" : a
                        .get("geojson").unwrap_or(& Value::Null) }
                    ),
                )
                .await?;
            Ok(json!({ "id" : id }))
        }
        "get_layer_geojson" => b.rpc(name, fields(&a, &["layer_id", "map_id"])).await,
        "search_layer_features" => {
            b.rpc(
                "mcp_layer_features",
                fields(&a, &["layer_id", "map_id", "query", "limit", "offset"]),
            )
            .await
        }
        "get_layer_feature" => {
            text(&a, "id").parse::<i64>()?;
            let mut args = fields(&a, &["layer_id", "map_id", "id"]);
            args["p_limit"] = json!(1);
            let d = b.rpc("mcp_layer_features", args).await?;
            Ok(d.as_array().and_then(|v| v.first()).map_or_else(
                || json!({ "status" : "missing" }),
                |f| json!({ "status" : "ok", "feature" : f }),
            ))
        }
        "get_my_profile" => {
            b.table(
                "profiles",
                Method::GET,
                None,
                &[
                    ("select", "id,display_name,created_at,updated_at".into()),
                    ("id", format!("eq.{}", text(&b.user, "id"))),
                ],
                true,
                false,
            )
            .await
        }
        "update_my_profile" => {
            b.table(
                "profiles",
                Method::PATCH,
                Some(&a),
                &[
                    ("select", "id,display_name".into()),
                    ("id", format!("eq.{}", text(&b.user, "id"))),
                ],
                true,
                false,
            )
            .await
        }
        "list_images" => {
            b.storage(
                Method::POST,
                &format!("object/list/{}", text(&a, "bucket")),
                Some(json!(
                    { "prefix" : a["prefix"], "search" : a["search"], "limit" :
                    a["limit"], "offset" : a["offset"], "sortBy" : { "column" :
                    "name", "order" : "asc" } }
                )),
            )
            .await
        }
        "get_image_url" | "upload_image" | "delete_image" => image(b, name, &a).await,
        "get_meeting" | "get_public_document" => b.rpc(name, fields(&a, &["id"])).await,
        "import_meeting_records" => {
            if serde_json::to_vec(&a["records"])?.len() > 2 * 1024 * 1024 {
                bail!("Maximum import size: 2 MiB");
            }
            b.rpc(
                name,
                fields(&a, &["meeting_id", "provider", "records", "dry_run"]),
            )
            .await
        }
        "set_meeting_vote_outcome" => {
            b.rpc(
                name,
                fields(
                    &a,
                    &[
                        "vote_id",
                        "outcome",
                        "source_url",
                        "document_sha256",
                        "locator",
                        "supersedes_id",
                        "correction_reason",
                    ],
                ),
            )
            .await
        }
        "get_person_meeting_votes" => {
            let mut args = fields(&a, &["person_id", "offset", "limit"]);
            for key in ["organization_id", "from", "to"] {
                if let Some(v) = a.get(key) {
                    args[format!("p_{key}")] = v.clone();
                }
            }
            b.rpc(name, args).await
        }
        "upload_meeting_attachment"
        | "get_meeting_attachment_url"
        | "begin_meeting_attachment_upload"
        | "finalize_meeting_attachment_upload"
        | "manage_meeting_attachment_upload" => crate::uploads::call(b, name, &a).await,
        "manage_election_results" => election_results(b, &a["operation"]).await,
        "get_georgia_legislative_source" => crate::georgia::read(b, &a).await,
        "browse_legislation" => {
            b.rpc(
                name,
                fields(
                    &a,
                    &[
                        "table",
                        "id",
                        "filters",
                        "query",
                        "limit",
                        "offset",
                        "bill_id",
                        "person_id",
                        "vote_id",
                    ],
                ),
            )
            .await
        }
        "get_legislative_vote_totals" => b.rpc(name, fields(&a, &["vote_id"])).await,
        "get_person_legislative_record" => {
            b.rpc(
                name,
                fields(
                    &a,
                    &[
                        "person_id",
                        "session_id",
                        "body_id",
                        "category",
                        "term_id",
                        "bill_query",
                        "limit",
                        "offset",
                    ],
                ),
            )
            .await
        }
        "import_legislation" => import_legislation(b, &a).await,
        "describe_legislative_resource" => Ok(catalog::legislation()[text(&a, "table")].clone()),
        "save_legislative_record" => save_legislation(b, &a).await,
        "delete_legislative_record" => delete_table(b, text(&a, "table"), &a, &["id"]).await,
        "list_offices" => {
            let mut q = vec![
                ("select", "*".into()),
                ("order", "name.asc,id.asc".into()),
                ("offset", a["offset"].to_string()),
                ("limit", a["limit"].to_string()),
            ];
            for k in ["organization_id", "id"] {
                if let Some(v) = a[k].as_str() {
                    q.push((k, format!("eq.{v}")));
                }
            }
            b.table("offices", Method::GET, None, &q, false, false)
                .await
        }
        "list_public_documents" => {
            b.rpc(
                name,
                json!(
                    { "p_query" : a.get("query").unwrap_or(& json!("")),
                    "p_meeting_id" : a.get("meeting_id").unwrap_or(& Value::Null),
                    "p_admin" : a.get("include_unpublished").unwrap_or(&
                    Value::Bool(false)), "p_limit" : a.get("limit").unwrap_or(&
                    json!(20)), "p_offset" : a.get("offset").unwrap_or(& json!(0)) }
                ),
            )
            .await
        }
        "link_public_document_meeting" => {
            b.rpc(name, fields(&a, &["document_id", "meeting_id"]))
                .await
        }
        "search_document_contents" | "publish_meeting_document" | "withdraw_public_document" => {
            document(b, name, &a).await
        }
        _ if name.starts_with("save_") => save(b, &name[5..], &a["request"]).await,
        _ if name.starts_with("delete_") => delete(b, &name[7..], &a).await,
        _ => bail!("Unknown tool: {name}"),
    }
}
async fn county_catalog(b: &Backend) -> Result<Value> {
    let mut data = json!({});
    for table in [
        "county_geographies",
        "county_statistic_topics",
        "county_statistic_indicators",
        "county_statistic_sources",
    ] {
        data[table] = b
            .table(
                table,
                Method::GET,
                None,
                &[("select", "*".into())],
                false,
                false,
            )
            .await?;
    }
    Ok(data)
}
async fn get_record(b: &Backend, kind: &str, id: Value) -> Result<Value> {
    if ["map", "event", "person", "election"].contains(&kind) {
        let mut data = b
            .rpc(&format!("get_{kind}"), json!({ "p_id" : id }))
            .await?;
        if kind == "map" && data["status"] == "ok" {
            data["layers"] = b.rpc("get_map_layers", json!({ "p_map_id" : id })).await?;
        }
        Ok(data)
    } else {
        b.rpc("mcp_get_record", json!({ "p_type" : kind, "p_id" : id }))
            .await
    }
}
async fn search(b: &Backend, a: &Value) -> Result<Value> {
    let kind = text(a, "type");
    let filters = ["organization_id", "calendar_id"]
        .iter()
        .any(|k| !a[*k].is_null())
        || a.get("include_past").is_some();
    if a["mode"] == "site" {
        if text(a, "query").chars().count() > 500 {
            bail!("Site query must be at most 500 characters");
        }
        if filters {
            bail!("Event filters require mode=directory and type=event");
        }
        if kind == "county_statistic_catalog" {
            return county_catalog(b).await;
        }
        let mut args = fields(a, &["query", "offset"]);
        args["p_limit"] = a.get("limit").cloned().unwrap_or(json!(20));
        if kind != "county_statistic" {
            args["p_type"] = a.get("type").cloned().unwrap_or(Value::Null);
        }
        return b
            .rpc(
                if kind == "county_statistic" {
                    "search_county_statistics"
                } else {
                    "search_site"
                },
                args,
            )
            .await;
    }
    let rpc = match kind {
        "organization" => "search_organizations",
        "person" => "search_people",
        "map" => "search_maps",
        "election" => "search_elections",
        "event" => "search_events",
        _ => {
            bail!(
                "Directory mode requires type=organization, person, map, election or event; use site mode for other types"
            )
        }
    };
    if a["offset"] != 0 {
        bail!("Directory mode does not support offset; use site mode for pagination");
    }
    if kind != "event" && filters {
        bail!("Event filters require type=event");
    }
    let max = if ["person", "organization"].contains(&kind) {
        50
    } else {
        100
    };
    if a["limit"].as_u64().is_some_and(|n| n > max) {
        bail!("Directory limit for {kind} must be at most {max}");
    }
    let mut args = json!(
        { "p_query" : a["query"], "p_limit" : a.get("limit").cloned().unwrap_or(json!(if
        ["event", "election"].contains(& kind) { 50 } else { 20 })) }
    );
    if kind == "event" {
        args["p_organization_id"] = a["organization_id"].clone();
        args["p_calendar_id"] = a["calendar_id"].clone();
        args["p_include_past"] = a.get("include_past").cloned().unwrap_or(json!(false));
    }
    b.rpc(rpc, args).await
}
fn resource(kind: &str) -> Result<(&str, &str, Vec<&str>)> {
    Ok(match kind {
        "organization" => ("organizations", "id, slug, name", vec!["id"]),
        "calendar" => ("calendars", "id, slug, name", vec!["id"]),
        "event" => ("events", "id, title, starts_at", vec!["id"]),
        "person" => ("people", "id, slug, full_name", vec!["id"]),
        "election" => ("elections", "id, slug, name", vec!["id"]),
        "map" => (
            "maps",
            "id,slug,name,visibility,base_map,organization_id,view_options",
            vec!["id"],
        ),
        "layer" => (
            "layers",
            "id,name,source_type,source_url,visibility,style,presentation",
            vec!["id"],
        ),
        "contest" => (
            "contests",
            "id,election_id,office,district,seat,description,partisan,position",
            vec!["id"],
        ),
        "ballot_question" => (
            "ballot_questions",
            "id,election_id,title,question_text,description,jurisdiction,choices,source_url,position",
            vec!["id"],
        ),
        "article" => ("articles", "*", vec!["id"]),
        "layer_feature" => ("layer_features", "*", vec!["layer_id", "id"]),
        "office" => ("offices", "*", vec!["id"]),
        "office_term" => ("office_terms", "*", vec!["id"]),
        "map_layer" => ("map_layers", "*", vec!["map_id", "layer_id"]),
        "contest_candidate" => ("contest_candidates", "*", vec!["contest_id", "person_id"]),
        "person_organization" => (
            "person_organizations",
            "*",
            vec!["person_id", "organization_id"],
        ),
        "organization_relationship" => ("organization_relationships", "*", vec!["id"]),
        "person_relationship" => ("person_relationships", "*", vec!["id"]),
        "article_person" => ("article_people", "*", vec!["article_id", "person_id"]),
        "article_organization" => (
            "article_organizations",
            "*",
            vec!["article_id", "organization_id"],
        ),
        _ => bail!("Unknown resource"),
    })
}
async fn save(b: &Backend, kind: &str, a: &Value) -> Result<Value> {
    let update = a["operation"] == "update";
    let (table, columns, keys) = resource(kind)?;
    let data = without(
        a,
        &if update {
            let mut k = keys.clone();
            k.push("operation");
            k
        } else {
            vec!["operation"]
        },
    );
    if update && data.as_object().is_none_or(serde_json::Map::is_empty) {
        bail!("Provide at least one field to update");
    }
    if kind == "article" {
        let mut meta = without(&data, &["people", "organizations"]);
        if update {
            meta["id"] = a["id"].clone();
        }
        return Ok(json!(
            { "id" : b.rpc("mcp_save_article", json!({ "p_article" : meta, "p_people"
            : data.get("people").unwrap_or(& Value::Null), "p_organizations" : data
            .get("organizations").unwrap_or(& Value::Null) })). await ? }
        ));
    }
    if kind == "layer_feature" {
        if update {
            let id = text(a, "id").parse::<i64>()?;
            if id < 1 {
                bail!("ID exceeds bigint range");
            }
        }
        let mut args = json!(
            { "p_layer_id" : a["layer_id"], "p_feature" : without(& data,& ["layer_id"])
            }
        );
        if update {
            args["p_id"] = a["id"].clone();
        }
        return Ok(json!(
            { "id" : b.rpc("mcp_save_layer_feature", args). await ?, "layer_id" :
            a["layer_id"] }
        ));
    }
    validate_refinements(&data)?;
    let mut q = vec![("select", columns.into())];
    if update {
        for key in keys {
            q.push((key, format!("eq.{}", text(a, key))));
        }
    }
    b.table(
        table,
        if update { Method::PATCH } else { Method::POST },
        Some(&data),
        &q,
        true,
        false,
    )
    .await
}
fn validate_refinements(a: &Value) -> Result<()> {
    if let Some(s) = a["meeting_video_url"].as_str() {
        let u = url::Url::parse(s)?;
        if !u.username().is_empty() || u.password().is_some() || u.port().is_some() {
            bail!("Use a video URL without credentials or a custom port");
        }
    }
    Ok(())
}
async fn delete(b: &Backend, kind: &str, a: &Value) -> Result<Value> {
    let (table, _, keys) = resource(kind)?;
    if kind == "layer_feature" {
        text(a, "id").parse::<i64>()?;
    }
    delete_table(b, table, a, &keys).await
}
async fn delete_table(b: &Backend, table: &str, a: &Value, keys: &[&str]) -> Result<Value> {
    let columns = if table == "layer_features" {
        "layer_id".into()
    } else {
        keys.join(",")
    };
    let mut q = vec![("select", columns)];
    let mut result = json!({});
    for key in keys {
        q.push((*key, format!("eq.{}", text(a, key))));
        result[*key] = a[*key].clone();
    }
    let data = b
        .table(table, Method::DELETE, None, &q, false, false)
        .await?;
    result["deleted"] = json!(data.as_array().map_or(0, Vec::len));
    Ok(result)
}
async fn related(b: &Backend, a: &Value) -> Result<Value> {
    let relation = text(a, "relation");
    match relation {
        "map_layers" => b.rpc("get_map_layers", fields(a, &["map_id"])).await,
        "person_articles" | "person_candidacies" | "person_relationships" => {
            b.rpc(&format!("get_{relation}"), fields(a, &["person_id"]))
                .await
        }
        "organization_articles" | "organization_relationships" => {
            b.rpc(&format!("get_{relation}"), fields(a, &["organization_id"]))
                .await
        }
        "calendars" => {
            b.rpc("list_calendars", fields(a, &["organization_id"]))
                .await
        }
        "office_terms" => {
            if a["person_id"].is_null() && a["organization_id"].is_null() {
                bail!("Provide person_id or organization_id");
            }
            b.rpc(
                "get_office_terms",
                fields(a, &["person_id", "organization_id"]),
            )
            .await
        }
        "person_organizations" => {
            let d = b
                .rpc("get_person", json!({ "p_id" : a["person_id"] }))
                .await?;
            Ok(json!(
                { "status" : d["status"], "organizations" : d.get("organizations")
                .unwrap_or(& json!([])) }
            ))
        }
        "election_contests" | "election_ballot_questions" => {
            let field = relation.trim_start_matches("election_");
            let d = b
                .rpc("get_election", json!({ "p_id" : a["election_id"] }))
                .await?;
            Ok(json!(
                { "status" : d["status"], field : d.get(field).unwrap_or(& json!([]))
                }
            ))
        }
        "contest_candidates" | "contest_candidate" => {
            let d = b
                .rpc(
                    "mcp_get_record",
                    json!({ "p_type" : "contest", "p_id" : a["contest_id"] }),
                )
                .await?;
            let candidates = d["contest"]["candidates"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if relation == "contest_candidates" {
                Ok(json!({ "status" : d["status"], "candidates" : candidates }))
            } else {
                Ok(candidates
                    .iter()
                    .find(|c| c["person"]["id"] == a["person_id"])
                    .map_or_else(
                        || json!({ "status" : "missing" }),
                        |c| {
                            json!(
                                { "status" : "ok", "contest_id" : a["contest_id"],
                                "person_id" : a["person_id"], "candidate" : c }
                            )
                        },
                    ))
            }
        }
        "map_layer_links" | "article_people" | "article_organizations" => {
            b.admin().await?;
            let (table, columns, key) = match relation {
                "map_layer_links" => (
                    "map_layers",
                    "map_id,layer_id,position,style,visible_by_default",
                    "map_id",
                ),
                "article_people" => ("article_people", "article_id,person_id,role", "article_id"),
                _ => (
                    "article_organizations",
                    "article_id,organization_id,role",
                    "article_id",
                ),
            };
            let mut q = vec![
                ("select", columns.into()),
                (key, format!("eq.{}", text(a, key))),
            ];
            if relation == "map_layer_links" {
                q.push(("order", "position.asc,layer_id.asc".into()));
            }
            b.table(table, Method::GET, None, &q, false, false).await
        }
        _ => bail!("Unknown relation"),
    }
}
async fn image(b: &Backend, name: &str, a: &Value) -> Result<Value> {
    let bucket = text(a, "bucket");
    let path = text(a, "path");
    if path
        .split('/')
        .any(|s| s.is_empty() || s == "." || s == "..")
    {
        bail!("Use a relative object path without dot segments");
    }
    if name == "delete_image" {
        b.remove_object(bucket, path).await?;
        return Ok(json!({ "deleted" : true, "bucket" : bucket, "path" : path }));
    }
    if name == "upload_image" {
        let bytes = crate::uploads::decode(text(a, "data_base64"), 2_097_152)?;
        b.upload(
            bucket,
            path,
            bytes,
            text(a, "content_type"),
            a["overwrite"] == true,
        )
        .await?;
    }
    Ok(json!({ "path" : path, "url" : b.public_url(bucket, path) ? }))
}
async fn document(b: &Backend, name: &str, a: &Value) -> Result<Value> {
    let base =
        b.kb.as_ref()
            .context("The public document archive is not configured yet")?;
    let path = match name {
        "search_document_contents" => "/search".into(),
        "publish_meeting_document" => "/publish".into(),
        _ => format!("/documents/{}/withdraw", text(a, "id")),
    };
    let mut url = base.join(&path)?;
    let empty = json!({});
    let request = if name == "search_document_contents" {
        url.query_pairs_mut().append_pair("q", text(a, "query"));
        b.client.get(url)
    } else {
        b.client
            .post(url)
            .bearer_auth(&b.token)
            .json(if name == "publish_meeting_document" {
                a
            } else {
                &empty
            })
    };
    Backend::send(request.timeout(std::time::Duration::from_secs(60))).await
}
async fn import_legislation(b: &Backend, a: &Value) -> Result<Value> {
    let rows = a["records"].as_array().context("records")?;
    let start = usize::try_from(a["start_offset"].as_u64().unwrap_or(0))?;
    if a["mode"] == "atomic" {
        if rows.len() > 100 || start != 0 {
            bail!(
                "Atomic imports require at most 100 records and start_offset=0; use mode=batched explicitly for larger imports"
            );
        }
        return b
            .rpc("import_legislation", json!({ "p_records" : rows }))
            .await;
    }
    if start > rows.len() {
        bail!("start_offset exceeds bundle length");
    }
    if serde_json::to_vec(rows)?.len() > 2_000_000 {
        bail!("Provide at most 2 MB of normalized records");
    }
    let mut offset = start;
    while offset < rows.len() {
        let end = (offset + 100).min(rows.len());
        if let Err(e) = b
            .rpc(
                "import_legislation",
                json!({ "p_records" : rows[offset..end] }),
            )
            .await
        {
            return Ok(json!(
                { "status" : "partial", "imported_count" : offset - start,
                "next_offset" : offset, "total" : rows.len(), "error" : e
                .to_string(), "retry_note" :
                "Retry this bundle from next_offset. Provider keys make a retry safe if the last response was uncertain."
                }
            ));
        }
        offset = end;
    }
    Ok(json!(
        { "status" : "complete", "imported_count" : offset - start, "next_offset" :
        offset, "total" : rows.len() }
    ))
}
async fn save_legislation(b: &Backend, a: &Value) -> Result<Value> {
    let table = text(a, "table");
    let update = a.get("id").is_some();
    let fields = catalog::legislation()[table]["fields"]
        .as_array()
        .context("Unknown resource")?;
    let mut properties = json!({});
    let mut required = vec![];
    for f in fields {
        let key = text(f, "name");
        let mut s = if !f["reference"].is_null() {
            json!({ "type" : "string", "format" : "uuid" })
        } else if f["type"] == "integer" {
            json!({ "type" : "integer" })
        } else {
            json!({ "type" : "string" })
        };
        if let Some(c) = f.get("choices") {
            s = json!({ "enum" : c });
        }
        if f["required"] != true {
            s = json!({ "anyOf" : [s, { "type" : "null" }] });
        } else if !update && f.get("default").is_none() {
            required.push(key);
        }
        properties[key] = s;
    }
    let mut data = a["data"].clone();
    catalog::validate(
        &json!(
            { "type" : "object", "properties" : properties, "required" : required,
            "additionalProperties" : false }
        ),
        &mut data,
    )?;
    if data.as_object().is_none_or(serde_json::Map::is_empty) {
        bail!("Provide at least one field");
    }
    let mut q = vec![("select", "*".into())];
    if update {
        q.push(("id", format!("eq.{}", text(a, "id"))));
    }
    b.table(
        table,
        if update { Method::PATCH } else { Method::POST },
        Some(&data),
        &q,
        true,
        false,
    )
    .await
}
async fn election_results(b: &Backend, a: &Value) -> Result<Value> {
    match text(a, "action") {
        "read" => {
            b.rpc(
                "get_election_results",
                fields(a, &["election_id", "history", "offset", "limit"]),
            )
            .await
        }
        "read_turnout" => {
            b.rpc(
                "get_structured_turnout",
                fields(a, &["election_id", "report_id", "offset", "limit"]),
            )
            .await
        }
        "import" => {
            b.rpc(
                "import_election_result",
                fields(
                    a,
                    &[
                        "election_id",
                        "scope",
                        "observed_at",
                        "snapshot",
                        "sources",
                        "expected_report_id",
                        "dry_run",
                    ],
                ),
            )
            .await
        }
        "import_turnout" => {
            let doc = text(a, "document");
            validate_turnout(doc)?;
            if crate::uploads::sha(doc.as_bytes()) != text(a, "sha256") {
                bail!("Evidence checksum mismatch");
            }
            b.rpc(
                "import_structured_turnout",
                fields(a, &["document", "sha256", "expected_report_id", "dry_run"]),
            )
            .await
        }
        "view" => Ok(json!(
            { "signedUrl" : b.signed_url("election-evidence", text(a, "path"),
            600, None). await ? }
        )),
        "upload" => {
            let bytes = crate::uploads::decode(text(a, "base64"), 10 * 1024 * 1024)?;
            let mime = text(a, "mime_type");
            let ext = match mime {
                "image/jpeg" if bytes.starts_with(&[255, 216, 255]) => "jpg",
                "image/png" if bytes.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]) => "png",
                "application/pdf" if bytes.starts_with(b"%PDF-") => "pdf",
                _ => bail!("File signature does not match MIME"),
            };
            let hash = crate::uploads::sha(&bytes);
            let path = format!("{hash}.{ext}");
            let result = b
                .upload("election-evidence", &path, bytes.clone(), mime, false)
                .await;
            let duplicate = result.is_err();
            if duplicate {
                let existing = b.download("election-evidence", &path).await?;
                if crate::uploads::sha(&existing) != hash {
                    bail!("Stored evidence checksum mismatch");
                }
            }
            Ok(json!(
                { "path" : path, "sha256" : hash, "bytes" : bytes.len(), "duplicate"
                : duplicate }
            ))
        }
        _ => bail!("Unknown action"),
    }
}
fn validate_turnout(document: &str) -> Result<()> {
    if document.len() > 2 * 1024 * 1024 {
        bail!("JSON evidence exceeds 2 MiB");
    }
    let mut data: Value = serde_json::from_str(document)?;
    catalog::validate(
        &serde_json::from_str(include_str!("../reference/turnout.json"))?,
        &mut data,
    )?;
    let tables = data["tables"].as_array().context("tables")?;
    if tables
        .iter()
        .map(|t| t["rows"].as_array().map_or(0, Vec::len))
        .sum::<usize>()
        > 795
    {
        bail!("Import requires at most 795 source rows");
    }
    let mut seen = std::collections::HashSet::new();
    for table in data["tables"].as_array().context("tables")? {
        let name = text(table, "table");
        if !seen.insert(name) {
            bail!("Duplicate table");
        }
        let (id, names) = match name {
            "early" => ("7cc869cd-2124-43be-82f4-1c8d394dc6d8", vec!["accepted"]),
            "absentee" => (
                "CTgPMg",
                vec![
                    "requested",
                    "issued",
                    "returned",
                    "accepted",
                    "rejected",
                    "spoiled_canceled",
                    "outstanding",
                    "acceptance_ratio",
                ],
            ),
            _ => ("QDFdpV", vec!["turnout"]),
        };
        if table["object_id"] != id {
            bail!("Object identity mismatch");
        }
        for row in table["rows"].as_array().context("rows")? {
            let style = text(row, "ballot_style");
            let group = match style {
                "Early In-Person" => "advance",
                "Absentee" => "absentee_mail",
                "Election Day In-Person" => "election_day",
                _ => bail!("Invalid ballot style/counting group"),
            };
            if row["counting_group"] != group
                || (name == "early" && style != "Early In-Person")
                || (name == "absentee" && style != "Absentee")
            {
                bail!("Invalid ballot style/counting group");
            }
            let measures = row["measures"].as_object().context("measures")?;
            if measures.len() != names.len() || !names.iter().all(|n| measures.contains_key(*n)) {
                bail!("Measure schema changed");
            }
            for (key, cell) in measures {
                if let Some(v) = cell["value"].as_f64()
                    && ((key == "acceptance_ratio" && v > 1.0)
                        || (key != "acceptance_ratio" && v.fract() != 0.0))
                {
                    bail!("Invalid count or ratio");
                }
            }
        }
    }
    Ok(())
}
