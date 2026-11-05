use crate::{backend::Backend, handlers::text, simbli, uploads};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
fn array<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v[key].as_array().map_or(&[], Vec::as_slice)
}
fn pick(v: &Value, keys: &[&str]) -> Value {
    keys.iter()
        .filter_map(|k| v.get(*k).map(|v| ((*k).to_owned(), v.clone())))
        .collect()
}
fn key_for(attachment: &Value, locator: &str, fallback: String) -> String {
    attachment["source_keys"][locator]
        .as_str()
        .map_or(fallback, str::to_owned)
}
fn person(v: &Value) -> &str {
    v["person_id"].as_str().unwrap_or("undefined")
}
fn jsonb_bytes(v: &Value) -> Result<usize> {
    fn spaces(v: &Value) -> usize {
        match v {
            Value::Object(o) => {
                o.len() + o.len().saturating_sub(1) + o.values().map(spaces).sum::<usize>()
            }
            Value::Array(a) => a.len().saturating_sub(1) + a.iter().map(spaces).sum::<usize>(),
            _ => 0,
        }
    }
    Ok(serde_json::to_vec(v)?.len() + spaces(v))
}
fn expand(provenance: &Value, attendance: &[Value], motions: &[Value]) -> Result<Value> {
    let mut rows = vec![];
    for a in attendance {
        let mut p = provenance.clone();
        p["locator"] = a["locator"].clone();
        rows.push(json!(
            { "kind" : "attendance", "source_key" : a["source_key"], "provenance" :
            p, "data" : pick(a,& ["person_id", "status", "evidence"]) }
        ));
    }
    for m in motions {
        let mut p = provenance.clone();
        p["locator"] = m["locator"].clone();
        rows.push(json!(
            { "kind" : "vote", "source_key" : m["source_key"], "provenance" : p,
            "data" : pick(m,& ["subject", "description"]) }
        ));
        for b in array(m, "ballots") {
            let mut data = pick(b, &["person_id", "choice"]);
            data["vote_source_key"] = m["source_key"].clone();
            data["original_text"] = b["text"].clone();
            rows.push(json!(
                { "kind" : "choice", "source_key" : b.get("source_key").cloned()
                .unwrap_or_else(|| json!(format!("{}:person:{}", text(m,
                "source_key"), person(b)))), "provenance" : p, "data" : data }
            ));
        }
    }
    let rows = json!(rows);
    let array = rows.as_array().context("observations")?;
    if array.is_empty() || array.len() > 2000 || jsonb_bytes(&rows)? > 2 * 1024 * 1024 {
        bail!("Maximum 2000 expanded observations / 2 MiB PostgreSQL JSONB text");
    }
    let keys: HashSet<_> = array.iter().map(|r| text(r, "source_key")).collect();
    if keys.len() != array.len() {
        bail!("Duplicate source keys");
    }
    Ok(rows)
}
#[allow(clippy::too_many_lines)] // Compare every stored motion and named ballot without discarding evidence.
fn reconciliation(parsed: &Value, motions: &[Value], sources: &[Value], hash: &str) -> Vec<Value> {
    array(parsed, "motions")
        .iter()
        .zip(motions)
        .map(|(m, motion)| {
            let stored = sources
                .iter()
                .find(|s| s["kind"] == "vote" && s["source_key"] == motion["source_key"]);
            let stored_ballots: Vec<_> = stored
                .map(|s| {
                    sources
                        .iter()
                        .filter(|b| b["kind"] == "choice" && b["vote_id"] == s["vote_id"])
                        .collect()
                })
                .unwrap_or_default();
            let mut differences = vec![];
            if let Some(s) = stored {
                for f in ["subject", "description"] {
                    if s["data"].get(f) != motion.get(f) {
                        differences.push(f.to_owned());
                    }
                }
                if s["provenance"]["document_sha256"] != hash {
                    differences.push("document_sha256".into());
                }
            }
            let mut matched = HashSet::new();
            let mut missing = vec![];
            let mut ballot_differences = vec![];
            for (j, b) in array(motion, "ballots").iter().enumerate() {
                let mut expected = pick(b, &["person_id", "choice"]);
                expected["source_key"] = b.get("source_key").cloned().unwrap_or_else(|| {
                    json!(format!(
                        "{}:person:{}",
                        text(motion, "source_key"),
                        person(b)
                    ))
                });
                expected["source_name"] = m["ballots"][j]["name"].clone();
                expected["original_text"] = b["text"].clone();
                let index = stored_ballots
                    .iter()
                    .enumerate()
                    .find(|(i, s)| {
                        !matched.contains(i)
                            && s.get("person_id") == expected.get("person_id")
                            && s["source_key"] == expected["source_key"]
                    })
                    .map(|(i, _)| i);
                if let Some(index) = index {
                    matched.insert(index);
                    let prior = stored_ballots[index];
                    if prior["data"].get("choice") != expected.get("choice")
                        || prior["data"].get("original_text") != expected.get("original_text")
                    {
                        let mut diff = pick(&expected, &["person_id", "source_key", "source_name"]);
                        diff["expected"] = pick(&expected, &["choice", "original_text"]);
                        diff["stored"] = pick(&prior["data"], &["choice", "original_text"]);
                        ballot_differences.push(diff);
                    }
                } else {
                    missing.push(expected);
                }
            }
            let unexpected: Vec<_> = stored_ballots
                .iter()
                .enumerate()
                .filter(|(i, _)| !matched.contains(i))
                .map(|(_, s)| {
                    let mut row = pick(s, &["person_id", "source_key"]);
                    for f in ["choice", "original_text"] {
                        if let Some(v) = s["data"].get(f) {
                            row[f] = v.clone();
                        }
                    }
                    row
                })
                .collect();
            for b in &missing {
                differences.push(format!("missing_ballot:{}", text(b, "source_key")));
            }
            for b in &unexpected {
                differences.push(format!("unexpected_ballot:{}", text(b, "source_key")));
            }
            for b in &ballot_differences {
                differences.push(format!("ballot:{}", text(b, "source_key")));
            }
            let conflict = differences
                .iter()
                .any(|d| !d.starts_with("missing_ballot:"));
            json!(
                { "locator" : m["locator"], "source_key" : motion["source_key"],
                "vote_id" : stored.and_then(| s | s.get("vote_id")).unwrap_or(&
                Value::Null), "status" : if stored.is_none() { "missing" } else if
                conflict { "differs" } else if ! missing.is_empty() { "partial" } else {
                "complete" }, "differences" : differences, "missing_ballots" : missing,
                "unexpected_ballots" : unexpected, "ballot_differences" :
                ballot_differences, "stored_ballots" : stored_ballots.len(),
                "document_ballots" : array(m, "ballots").len() }
            )
        })
        .collect()
}
#[allow(clippy::too_many_lines)]
pub async fn import(b: &Backend, a: &Value) -> Result<Value> {
    let meeting = b
        .rpc("get_meeting", json!({ "p_id" : a["meeting_id"] }))
        .await?;
    if meeting["status"] != "ok" {
        bail!("Meeting unavailable");
    }
    let mut motions = array(a, "motions").to_vec();
    let mut attendance = array(a, "attendance").to_vec();
    let mut warnings = vec![];
    let mut document = Value::Null;
    let sources: Vec<_> = array(&meeting, "sources")
        .iter()
        .filter(|s| s["provider"] == a["provider"])
        .cloned()
        .collect();
    if let Some(attachment) = a.get("attachment") {
        if !motions.is_empty() || !attendance.is_empty() {
            bail!("Attachment and inline observations are mutually exclusive");
        }
        let stored = array(&meeting, "attachments")
            .iter()
            .find(|s| s["id"] == attachment["id"] && s["meeting_id"] == a["meeting_id"])
            .context("Matching bounded TXT/JSON attachment required")?;
        let path = text(stored, "storage_path");
        let size = stored["size_bytes"].as_u64().unwrap_or(0);
        if !["text/plain", "application/json"].contains(&text(stored, "mime_type"))
            || size == 0
            || size > 2 * 1024 * 1024
            || !path.starts_with(&format!("{}/", text(a, "meeting_id")))
            || path
                .chars()
                .any(|c| c == '?' || c == '#' || c == '\\' || c < ' ')
            || path
                .split('/')
                .any(|s| s.is_empty() || s == "." || s == "..")
        {
            bail!("Matching bounded TXT/JSON attachment required");
        }
        if stored
            .get("sha256")
            .is_some_and(|s| !s.is_null() && *s != a["provenance"]["document_sha256"])
        {
            bail!("Attachment checksum metadata conflict");
        }
        let bytes = b.download("meeting-attachments", path).await?;
        if bytes.len() as u64 != size {
            bail!("Attachment size mismatch");
        }
        let hash = uploads::sha(&bytes);
        if a["provenance"]["document_sha256"] != hash {
            bail!("Original SHA-256 mismatch");
        }
        let parsed = simbli::parse(std::str::from_utf8(&bytes)?)?;
        warnings = array(&parsed, "warnings").to_vec();
        if !array(&parsed, "source_urls")
            .iter()
            .any(|s| *s == a["provenance"]["source_url"])
        {
            bail!("Source URL must match captured Minutes source URL exactly");
        }
        let mut people = HashMap::new();
        for p in array(attachment, "people") {
            if people
                .insert(text(p, "name"), p["person_id"].clone())
                .is_some()
            {
                bail!("Duplicate source name binding");
            }
        }
        let mut names = HashSet::new();
        let mut missing = vec![];
        for m in array(&parsed, "motions") {
            for p in array(m, "ballots").iter().chain(array(m, "absences")) {
                let name = text(p, "name");
                if names.insert(name) && !people.contains_key(name) {
                    missing.push(name);
                }
            }
        }
        document = json!(
            { "attachment_id" : stored["id"], "sha256" : hash, "adapter" :
            parsed["adapter"], "motions" : array(& parsed, "motions").len(),
            "named_ballots" : array(& parsed, "motions").iter().flat_map(| m | array(m,
            "ballots")).filter(| b | b["choice"] != "unknown").count(), "unknown_choices"
            : array(& parsed, "motions").iter().flat_map(| m | array(m, "ballots"))
            .filter(| b | b["choice"] == "unknown").count() }
        );
        motions = array(&parsed, "motions")
            .iter()
            .map(|m| {
                let locator = text(m, "locator");
                let ballots: Vec<_> = array(m, "ballots")
                    .iter()
                    .map(|p| {
                        let mut ballot = pick(p, &["choice", "text"]);
                        if let Some(id) = people.get(text(p, "name")) {
                            ballot["person_id"] = id.clone();
                        }
                        if let Some(key) =
                            attachment["source_keys"].get(format!("{locator}:{}", text(p, "name")))
                        {
                            ballot["source_key"] = key.clone();
                        }
                        ballot
                    })
                    .collect();
                json!(
                    { "source_key" : key_for(attachment, locator,
                    format!("simbli:{}:round:{}", text(m, "item_id"), text(m, "round"))),
                    "locator" : locator, "subject" : m["subject"], "description" :
                    m["description"], "ballots" : ballots }
                )
            })
            .collect();
        let reconciliation = reconciliation(&parsed, &motions, &sources, &hash);
        let unbound = sources.iter().any(|s| s["kind"] == "vote")
            && array(&parsed, "motions")
                .iter()
                .any(|m| attachment["source_keys"].get(text(m, "locator")).is_none());
        if a["reconcile_only"] == true || !missing.is_empty() || unbound || !warnings.is_empty() {
            let existing: Vec<_> = sources
                .iter()
                .filter(|s| s["kind"] == "vote")
                .map(|s| {
                    let mut value = pick(s, &["source_key", "vote_id"]);
                    if let Some(subject) = s["data"].get("subject") {
                        value["subject"] = subject.clone();
                    }
                    value["ballots"] = json!(
                        sources
                            .iter()
                            .filter(|b| b["kind"] == "choice" && b["vote_id"] == s["vote_id"])
                            .count()
                    );
                    value
                })
                .collect();
            let mut result = json!(
                { "status" : "review_required", "document" : document, "missing_people" :
                missing, "warnings" : warnings, "reconciliation" : reconciliation,
                "existing_motions" : existing }
            );
            if unbound {
                result["reason"] = json!(
                    "Bind every document motion locator to its existing source key before importing into an already imported meeting"
                );
            }
            return Ok(result);
        }
        let mut observed = HashMap::new();
        let mut observed_order = vec![];
        for m in &motions {
            for ballot in array(m, "ballots") {
                let id = person(ballot);
                if ballot["choice"] != "unknown" && !observed.contains_key(id) {
                    observed_order.push(id);
                    observed.insert(
                        id,
                        json!(
                            { "evidence" : ballot["text"], "locator" : m["locator"] }
                        ),
                    );
                }
            }
        }
        let mut absent = HashMap::new();
        let mut absent_order = vec![];
        for m in array(&parsed, "motions") {
            for p in array(m, "absences") {
                let id = people
                    .get(text(p, "name"))
                    .and_then(Value::as_str)
                    .context("Missing person binding")?;
                if !observed.contains_key(id) && !absent.contains_key(id) {
                    absent_order.push(id);
                    absent.insert(
                        id,
                        json!({ "evidence" : p["text"], "locator" : m["locator"] }),
                    );
                }
            }
        }
        attendance = observed_order
            .iter()
            .map(|id| {
                let e = &observed[id];
                json!(
                    { "source_key" : key_for(attachment,& format!("attendance:{id}"),
                    format!("simbli:attendance:{id}")), "person_id" : id, "status" :
                    "observed", "evidence" : e["evidence"], "locator" : e["locator"] }
                )
            })
            .chain(absent_order.iter().map(|id| {
                let e = &absent[id];
                json!(
                    { "source_key" : key_for(attachment,&
                    format!("attendance:{id}"),
                    format!("simbli:attendance:{id}")), "person_id" : id,
                    "status" : "absent", "evidence" : e["evidence"], "locator" :
                    e["locator"] }
                )
            }))
            .collect();
    }
    if a["reconcile_only"] == true {
        return Ok(json!(
            { "status" : "inventory_only", "observations" : attendance.len() +
            motions.iter().map(| m | 1 + array(m, "ballots").len()).sum::< usize >
            (), "existing" : sources.len(), "existing_sources" : sources.iter().map(|
            s | pick(s,& ["kind", "source_key", "vote_id", "person_id"])).collect::<
            Vec < _ >> (), "comparison_performed" : false }
        ));
    }
    let rows = expand(&a["provenance"], &attendance, &motions)?;
    let data = b
        .rpc(
            "import_meeting_records_compact",
            json!(
                { "p_meeting_id" : a["meeting_id"], "p_provider" : a["provider"],
                "p_records" : rows, "p_dry_run" : a["dry_run"] }
            ),
        )
        .await?;
    let mut result = data;
    let errors = array(&result, "errors").to_vec();
    result["errors"] = json!(errors.iter().take(25).collect::<Vec<_>>());
    result["errors_truncated"] = json!(errors.len() > 25);
    result["document"] = document;
    result["warnings"] = json!(warnings);
    Ok(result)
}
