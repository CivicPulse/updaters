//! Caller-authorized election lookup and bounded official Georgia result reads.
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, NaiveDate, Utc};
use reqwest::{Client, header::HeaderValue};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

// Only the statewide scope is deserialized; county precinct detail is skipped.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OfficialFeed {
    election_date: String,
    election_name: String,
    created_at: String,
    results: Value,
}
struct CachedFeed {
    source: String,
    fetched: std::time::Instant,
    feed: Value,
    hash: String,
    retrieved_at: String,
}
static CACHE: tokio::sync::Mutex<Option<CachedFeed>> = tokio::sync::Mutex::const_new(None);

const MAX_BYTES: usize = 64 * 1024 * 1024;

/// Resolve an election using the caller's RLS permissions, then read its official feed.
///
/// # Errors
/// Rejects inaccessible elections, unsupported sources and invalid or oversized feeds.
pub async fn read(
    client: &Client,
    base: &reqwest::Url,
    key: &HeaderValue,
    bearer: &HeaderValue,
    election_id: &str,
    query: &str,
) -> Result<Value> {
    uuid::Uuid::parse_str(election_id).context("election_id must be a UUID")?;
    ensure!(query.len() <= 200, "query is too long");
    let response = client
        .post(base.join("/rest/v1/rpc/get_election")?)
        .header("apikey", key.clone())
        .header(reqwest::header::AUTHORIZATION, bearer.clone())
        .json(&json!({"p_id":election_id}))
        .send()
        .await?
        .error_for_status()?;
    let record: Value = response.json().await?;
    let election = record.get("election").context("Election unavailable")?;
    let source = election["results_json_url"]
        .as_str()
        .context("No supported official results source configured")?;
    let date = election["election_date"]
        .as_str()
        .or_else(|| election["date"].as_str())
        .context("Election date unavailable")?;
    let expected_date = NaiveDate::parse_from_str(date, "%Y-%m-%d")?;
    validate_source(source, expected_date)?;
    // Serialize source refreshes and reuse a bounded, public-only snapshot for 60 seconds.
    let mut cache = CACHE.lock().await;
    if let Some(cached) = cache.as_ref()
        && cached.source == source
        && cached.fetched.elapsed().as_secs() < 60
    {
        let mut result = summarize(&cached.feed, date, source, &cached.hash, query)?;
        result["retrieved_at"] = json!(cached.retrieved_at);
        return Ok(result);
    }
    // A dedicated client prevents caller credentials from reaching the public source.
    let public = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(60))
        .connect_timeout(std::time::Duration::from_secs(5))
        .user_agent("CivicPulse-ElectionResults/1.0")
        .build()?;
    let mut response = public.get(source).send().await?;
    ensure!(
        response.status().is_success(),
        "Official results source unavailable"
    );
    ensure!(
        response
            .content_length()
            .is_none_or(|n| n <= MAX_BYTES as u64),
        "Official results source too large"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= MAX_BYTES,
            "Official results source too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    let (feed, hash) = tokio::task::spawn_blocking(move || -> Result<_> {
        let hash = format!("{:x}", Sha256::digest(&bytes));
        let parsed: OfficialFeed =
            serde_json::from_slice(&bytes).context("Invalid official results JSON")?;
        Ok((
            json!({"electionDate":parsed.election_date,"electionName":parsed.election_name,
            "createdAt":parsed.created_at,"results":parsed.results}),
            hash,
        ))
    })
    .await??;
    let result = summarize(&feed, date, source, &hash, query)?;
    *cache = Some(CachedFeed {
        source: source.to_owned(),
        fetched: std::time::Instant::now(),
        feed,
        hash,
        retrieved_at: result["retrieved_at"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    });
    Ok(result)
}

fn validate_source(source: &str, date: NaiveDate) -> Result<()> {
    let url = reqwest::Url::parse(source)?;
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("results.sos.ga.gov")
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path()
                == format!(
                    "/cdn/results/Georgia/export-{}GeneralAndSpecialElection.json",
                    date.format("%m%d%Y")
                ),
        "Unsupported official results source"
    );
    Ok(())
}

/// Summarize the statewide scope only. County scopes must never be added to it.
///
/// # Errors
/// Rejects wrong election dates, missing source timestamps and invalid vote counts.
#[allow(clippy::cast_precision_loss)] // Percentages are approximate; exact integer counts remain in the response.
pub fn summarize(feed: &Value, date: &str, source: &str, hash: &str, query: &str) -> Result<Value> {
    ensure!(
        feed["electionDate"].as_str() == Some(date),
        "Official election date mismatch"
    );
    let timestamp = feed["createdAt"]
        .as_str()
        .context("Missing official source timestamp")?;
    let time = DateTime::parse_from_rfc3339(timestamp)?;
    ensure!(
        time <= Utc::now() + chrono::Duration::minutes(5),
        "Future official source timestamp"
    );
    let scope = &feed["results"];
    let items = scope["ballotItems"]
        .as_array()
        .context("Missing statewide contests")?;
    let needle = query.trim().to_lowercase();
    let mut contests = Vec::new();
    for item in items {
        let options = item["ballotOptions"]
            .as_array()
            .context("Missing contest choices")?;
        let matched = needle.is_empty()
            || item["name"]
                .as_str()
                .unwrap_or("")
                .to_lowercase()
                .contains(&needle)
            || options.iter().any(|o| {
                o["name"]
                    .as_str()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&needle)
            });
        if !matched {
            continue;
        }
        let mut total = 0_u64;
        let mut complete = true;
        for option in options {
            if option["voteCount"].is_null() {
                complete = false;
                continue;
            }
            let votes = option["voteCount"]
                .as_u64()
                .context("Invalid official vote count")?;
            total = total.checked_add(votes).context("Vote total overflow")?;
        }
        let mut choices = Vec::new();
        for option in options {
            let votes = option["voteCount"].as_u64();
            let share = if complete && total > 0 {
                votes.map(|v| v as f64 * 100.0 / total as f64)
            } else {
                None
            };
            choices.push(json!({"source_id":option["id"],"name":option["name"],"party":option["politicalParty"],
                "votes":option["voteCount"],"vote_share_percent":share,"voting_methods":option["groupResults"]}));
        }
        contests.push(json!({"source_id":item["id"],"name":item["name"],"vote_for":item["voteFor"],
            "total_choice_votes":if complete {Some(total)} else {None},"choices":choices,
            "precincts_reporting":item["precinctsReporting"],"precincts_participating":item["precinctsParticipating"]}));
    }
    Ok(
        json!({"election_date":date,"election_name":feed["electionName"],"scope":scope["name"],
        "source_url":source,"source_updated_at":timestamp,"retrieved_at":Utc::now().to_rfc3339(),"source_sha256":hash,
        "certification_status":"not_established","reporting_statuses":scope["reportingStatuses"],
        "query":query,"contests":contests,
        "notes":["Reported totals; certification is not established by this reader.",
        "Vote share uses all choice votes in the contest, not ballots cast; multi-seat contests may allow multiple votes.",
        "An empty match does not establish that a candidate received zero votes."]}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Value {
        json!({"electionDate":"2026-11-03","createdAt":"2026-11-04T19:39:15Z","results":{"name":"Georgia","reportingStatuses":[{"status":"Not Reported"}],"ballotItems":[{"id":"SHD47","name":"House 47","voteFor":1,"ballotOptions":[{"name":"Jack Miller","voteCount":13675},{"name":"Lindsay Defranco","voteCount":11720}]}]},"localResults":[{"ballotItems":[]}]})
    }
    #[test]
    fn candidate_match_keeps_opponents_and_missing_precincts() {
        let r = summarize(&fixture(), "2026-11-03", "official", "hash", "DeFranco").unwrap();
        assert_eq!(r["contests"][0]["total_choice_votes"], 25395);
        assert_eq!(r["contests"][0]["choices"].as_array().unwrap().len(), 2);
        assert!(r["contests"][0]["precincts_reporting"].is_null());
        assert_eq!(r["certification_status"], "not_established");
        assert!(summarize(&fixture(), "2026-11-04", "official", "hash", "").is_err());
    }
    #[test]
    fn null_is_not_zero_and_bad_counts_fail() {
        let mut f = fixture();
        f["results"]["ballotItems"][0]["ballotOptions"][1]["voteCount"] = Value::Null;
        let r = summarize(&f, "2026-11-03", "official", "hash", "").unwrap();
        assert!(r["contests"][0]["total_choice_votes"].is_null());
        assert!(r["contests"][0]["choices"][0]["vote_share_percent"].is_null());
        f["results"]["ballotItems"][0]["ballotOptions"][1]["voteCount"] = json!(-1);
        assert!(summarize(&f, "2026-11-03", "official", "hash", "").is_err());
    }
    #[test]
    fn reject_redirect_targets_and_wrong_dates() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        assert!(validate_source("https://results.sos.ga.gov/cdn/results/Georgia/export-11032026GeneralAndSpecialElection.json",date).is_ok());
        assert!(validate_source("https://evil.test/cdn/results/Georgia/export-11032026GeneralAndSpecialElection.json",date).is_err());
        assert!(validate_source("https://results.sos.ga.gov/cdn/results/Georgia/export-11032026GeneralAndSpecialElection.json?redirect=x",date).is_err());
    }
    #[tokio::test]
    async fn visibility_lookup_forwards_only_caller_credentials_and_refuses_foreign_sources() {
        let upstream = axum::Router::new().route("/rest/v1/rpc/get_election", axum::routing::post(|headers: axum::http::HeaderMap, axum::Json(body): axum::Json<Value>| async move {
            assert_eq!(headers["apikey"], "sb_publishable_test");
            assert_eq!(headers["authorization"], "Bearer ordinary-user");
            assert_eq!(body["p_id"], "295d864d-80c8-4a87-ab16-3f4f3fa5ddb7");
            axum::Json(json!({"status":"ok","election":{"election_date":"2026-11-03","results_json_url":"http://127.0.0.1/private"}}))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base =
            reqwest::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });
        let result = read(
            &Client::new(),
            &base,
            &HeaderValue::from_static("sb_publishable_test"),
            &HeaderValue::from_static("Bearer ordinary-user"),
            "295d864d-80c8-4a87-ab16-3f4f3fa5ddb7",
            "",
        )
        .await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Unsupported official results source")
        );
        task.abort();
    }
}
