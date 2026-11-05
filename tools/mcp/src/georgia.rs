use crate::{backend::Backend, handlers::text};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha512};
const API: &str = "https://www.legis.ga.gov/api/";
#[allow(clippy::too_many_lines)] // Mirrors the official-source adapter and response projection.
pub async fn read(b: &Backend, a: &Value) -> Result<Value> {
    if let Some(reader) = &b.georgia {
        let mut url = reader.clone();
        for (key, value) in a.as_object().context("input")? {
            url.query_pairs_mut().append_pair(
                key,
                &value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned),
            );
        }
        let result = Backend::send(
            b.client
                .get(url)
                .timeout(std::time::Duration::from_secs(45)),
        )
        .await?;
        if result["source_system"] != "legis.ga.gov"
            || !text(&result, "source_url").starts_with(API)
            || !result["rows"].is_array()
            || !result["total"].is_u64()
        {
            bail!("Invalid Georgia source reader response");
        }
        return Ok(result);
    }
    let kind = text(a, "resource");
    let path = match kind {
        "sessions" => "sessions".into(),
        "vote" => {
            let id = a["vote_id"].as_u64().context("Supply vote_id")?;
            format!("Vote/detail/{id}")
        }
        _ => {
            let session = a["session_id"]
                .as_u64()
                .context("Supply session_id and chamber")?;
            let chamber = match text(a, "chamber") {
                "house" => 1,
                "senate" => 2,
                _ => bail!("Supply session_id and chamber"),
            };
            if kind == "votes" {
                format!("Vote/list/{chamber}/{session}")
            } else {
                format!("members/list/{session}?chamber={chamber}")
            }
        }
    };
    let ms = chrono::Utc::now().timestamp_millis();
    let key = format!(
        "{:x}",
        Sha512::digest(
            format!("QFpCwKfd7fjVEXFFwSu36BwwcP83xYgxLAhLYmKkletvarconst{ms}").as_bytes()
        )
    );
    let token = Backend::send(
        b.client
            .get(format!("{API}authentication/token?key={key}&ms={ms}"))
            .timeout(std::time::Duration::from_secs(20)),
    )
    .await?;
    let token = token
        .as_str()
        .filter(|s| !s.is_empty())
        .context("Invalid Georgia source handshake")?;
    let data = Backend::send(
        b.client
            .get(format!("{API}{path}"))
            .bearer_auth(token)
            .timeout(std::time::Duration::from_secs(20)),
    )
    .await?;
    let member = a.get("member_id");
    let rows = if kind == "vote" {
        data["votes"].as_array()
    } else {
        data.as_array()
    }
    .context("Invalid Georgia source response")?;
    let filtered: Vec<_> = rows
        .iter()
        .filter(|r| {
            member.is_none_or(|id| {
                if kind == "vote" {
                    r["member"]["id"] == *id
                } else {
                    r["id"] == *id
                }
            })
        })
        .cloned()
        .collect();
    let total = filtered.len();
    let offset = usize::try_from(a["offset"].as_u64().unwrap_or(0))?;
    let limit = usize::try_from(a["limit"].as_u64().unwrap_or(25))?;
    let rows: Vec<_> = filtered
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|mut r| {
            if kind == "vote" {
                let (choice, source) = match r["memberVoted"].as_u64() {
                    Some(0) => ("yes", "Yea".into()),
                    Some(1) => ("no", "Nay".into()),
                    Some(2) => ("excused", "Excused".into()),
                    Some(3) => ("not_voting", "NotVoting".into()),
                    Some(4) => ("other", "Unknown".into()),
                    _ => ("other", r["memberVoted"].to_string()),
                };
                r["choice"] = json!(choice);
                r["source_choice"] = json!(source);
            } else if kind == "votes" {
                let caption = text(&r, "caption").trim().to_lowercase();
                let caption = caption
                    .strip_prefix("morning ")
                    .or_else(|| caption.strip_prefix("afternoon "))
                    .or_else(|| caption.strip_prefix("evening "))
                    .unwrap_or(&caption);
                r["attendance"] = json!(
                    r["id"] == 21003
                        || r["isRollCall"] == true
                        || ["roll call", "attendance"].contains(&caption)
                );
            }
            r
        })
        .collect();
    let mut result = json!({"source_system":"legis.ga.gov","source_url":format!("{API}{path}"),"retrieved_at":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"total":total,"rows":rows});
    if kind == "vote" {
        result["session"] = data["session"].clone();
        result["legislation"] = data["legislation"].clone();
        result["member_identity_conflict"] = json!(member.is_some() && total > 1);
        if a["vote_id"] == 21003 {
            result["attendance"] = json!(true);
        }
        result["warning"] = json!(
            "isRollCall means attendance in this source, not a recorded legislative vote. Missing members are unknown. This endpoint does not import data or reconcile Civic Pulse identities."
        );
    }
    Ok(result)
}
