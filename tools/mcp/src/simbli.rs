//! Pure, versioned parser for captured Simbli JSON; HTML is reduced to text only.
use anyhow::{Context, Result, bail};
use regex::Regex;
use serde_json::{Value, json};
use std::collections::HashSet;
pub const ADAPTER: &str = "simbli-minutes-v1";
fn rx(pattern: &str) -> Regex {
    Regex::new(pattern).expect("literal parser regex")
}
fn string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Object(_) => "[object Object]".into(),
        _ => value.to_string(),
    }
}
pub fn source_text(value: &Value) -> String {
    let value = string(value);
    let value = rx(r"(?is)<head\b[^>]*>.*?</head>").replace_all(&value, "");
    let value = rx(r"(?is)<script\b[^>]*>.*?</script>").replace_all(&value, "");
    let value = rx(r"(?i)<(?:br\b[^>]*|/p|/div|/li)>").replace_all(&value, "\n");
    let value = rx(r"<[^>]*>").replace_all(&value, "");
    let mut value = value.into_owned();
    for (pattern, replacement) in [
        (r"(?i)&nbsp;|&#160;", " "),
        (r"(?i)&amp;", "&"),
        (r"(?i)&lt;", "<"),
        (r"(?i)&gt;", ">"),
        (r"(?i)&quot;", "\""),
        (r"(?i)&#39;|&apos;", "'"),
    ] {
        value = rx(pattern).replace_all(&value, replacement).into_owned();
    }
    rx(r"[ \t]+").replace_all(&value, " ").trim().to_owned()
}
fn choice(label: &str) -> &str {
    match label {
        "yes" => "yes",
        "no" => "no",
        "abstain" | "abstained" => "abstain",
        "recused" => "recused",
        _ => "unknown",
    }
}
type NamedEvidence = (Vec<Value>, Vec<Value>, HashSet<String>, usize);
fn named(votes: &str, reverse: bool, reject_duplicates: bool) -> Result<NamedEvidence> {
    let pattern = if reverse {
        r"(?i)(?:^|\n)\s*(Yes|No|Abstain(?:ed)?|Recused|Vote Not Recorded|Not Present|Absent)\s*:\s*([^\n]+)"
    } else {
        r"(?i)(?:^|\n)\s*([^\n:]+)\s*:\s*(Yes|No|Abstain(?:ed)?|Recused|Vote Not Recorded|Not Present|Absent)\b"
    };
    let mut ballots = vec![];
    let mut absences = vec![];
    let mut recognized = HashSet::new();
    let mut last = 0;
    for m in rx(pattern).captures_iter(votes) {
        let all = m.get(0).context("match")?;
        last = all.end();
        let name = m[if reverse { 2 } else { 1 }].trim();
        let label = m[if reverse { 1 } else { 2 }].to_lowercase();
        let label = label.trim();
        if reject_duplicates
            && ballots
                .iter()
                .chain(absences.iter())
                .any(|v: &Value| v["name"] == name)
        {
            bail!("Duplicate named ballot");
        }
        recognized.insert(all.as_str().trim().to_owned());
        let mut row = json!({ "name" : name, "text" : all.as_str().trim() });
        if ["not present", "absent"].contains(&label) {
            absences.push(row);
        } else {
            row["choice"] = json!(choice(label));
            ballots.push(row);
        }
    }
    Ok((ballots, absences, recognized, last))
}
fn warning(warnings: &mut Vec<Value>, locator: &str, reason: &str) {
    warnings.push(json!({ "locator" : locator, "reason" : reason }));
}
fn unparsed(text: &str, recognized: &HashSet<String>) -> bool {
    let colon = rx(r"^[^:\d]+:\s*[^\d]");
    let by = rx(r"(?i)motion (?:made|seconded) by");
    text.split('\n')
        .any(|line| colon.is_match(line) && !by.is_match(line) && !recognized.contains(line.trim()))
}
fn motion_like(text: &str) -> bool {
    rx(r"(?i)\bmotion\b|\bvoted\b").is_match(text)
}
fn named_set(rows: &[Value], kind: &str) -> Vec<String> {
    rows.iter()
        .map(|b| {
            json!([
                b["name"],
                b.get("choice").unwrap_or(&json!("unknown")),
                kind
            ])
            .to_string()
        })
        .collect()
}
#[allow(clippy::too_many_lines)]
fn walk(
    items: &Value,
    prefix: &str,
    motions: &mut Vec<Value>,
    warnings: &mut Vec<Value>,
) -> Result<()> {
    let items = items.as_array().context("Invalid minutes item list")?;
    for (i, item) in items.iter().enumerate() {
        let locator = format!("{prefix}[{i}]");
        if item.get("Visibility").is_some_and(|v| *v != 0) {
            warning(
                warnings,
                &locator,
                "Non-public minutes item requires review",
            );
            continue;
        }
        if let Some(rounds) = item["MeetingOnlineVotings"].as_array() {
            for (r, round) in rounds.iter().enumerate() {
                let path = format!("{locator}.MeetingOnlineVotings[{r}]");
                if round["IsDeleted"] == true {
                    warning(warnings, &path, "Deleted voting round requires review");
                    continue;
                }
                let minutes = source_text(&round["Minutes"]);
                let html = round["VotingHTML"]
                    .as_array()
                    .map(|v| {
                        v.iter()
                            .filter(|v| v.is_string())
                            .map(source_text)
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                let blocks: Vec<_> = rx(r"(?i)\bVoting\s*:\s*")
                    .find_iter(&minutes)
                    .map(|m| (m.start(), m.end()))
                    .collect();
                let segments: Vec<_> = if !blocks.is_empty() {
                    blocks
                        .iter()
                        .enumerate()
                        .map(|(j, (start, end))| {
                            (
                                minutes[if j == 0 { 0 } else { blocks[j - 1].1 }..*start]
                                    .trim()
                                    .to_owned(),
                                minutes[*end..blocks.get(j + 1).map_or(minutes.len(), |b| b.0)]
                                    .to_owned(),
                                format!(".Voting[{j}]"),
                                false,
                            )
                        })
                        .collect()
                } else if !html.is_empty() {
                    vec![(minutes.clone(), html.clone(), ".VotingHTML".into(), true)]
                } else {
                    vec![]
                };
                let mut parsed = 0;
                let round_start = motions.len();
                for (j, (description, votes, suffix, reverse)) in segments.iter().enumerate() {
                    let (ballots, absences, recognized, last) = named(votes, *reverse, true)
                        .with_context(|| format!("at {path}{suffix}"))?;
                    let at = format!("{path}{suffix}");
                    if unparsed(votes, &recognized) {
                        warning(
                            warnings,
                            &at,
                            "Unparsed colon-delimited voting text; review required",
                        );
                    }
                    if j == segments.len() - 1 && motion_like(&votes[last..]) {
                        warning(
                            warnings,
                            &at,
                            "Motion-like text after the final voting block requires review",
                        );
                    }
                    if ballots.is_empty() {
                        warning(warnings, &at, "Voting block has no supported named choices");
                    } else {
                        parsed += 1;
                        let description = rx(
                                r"(?mi)^[^\n:]+:\s*(?:Not Present|Vote Not Recorded|Abstain(?:ed)?|Recused|Absent|Yes|No)\s*$",
                            )
                            .replace_all(description, "")
                            .trim()
                            .to_owned();
                        motions.push(json!(
                            { "locator" : at, "item_id" : string(& item["EncrId"]),
                            "round" : format!("{r}:{j}"), "subject" : source_text(&
                            item["Title"]).chars().take(300).collect::< String > (),
                            "description" : description, "ballots" : ballots, "absences"
                            : absences }
                        ));
                    }
                }
                if !blocks.is_empty() && !html.trim().is_empty() {
                    let (ballots, absences, recognized, _) = named(&html, true, false)?;
                    let mut represented = named_set(&ballots, "ballot");
                    represented.extend(named_set(&absences, "absence"));
                    represented.sort();
                    let mut from_minutes = vec![];
                    for m in &motions[round_start..] {
                        from_minutes.extend(named_set(
                            m["ballots"].as_array().context("ballots")?,
                            "ballot",
                        ));
                        from_minutes.extend(named_set(
                            m["absences"].as_array().context("absences")?,
                            "absence",
                        ));
                    }
                    from_minutes.sort();
                    if represented != from_minutes || unparsed(&html, &recognized) {
                        warning(
                            warnings,
                            &path,
                            "Minutes and VotingHTML named evidence differ or cannot be aligned; reconcile both representations before import",
                        );
                    }
                    let by = rx(r"(?i)motion (?:made|seconded) by");
                    let other = html
                        .split('\n')
                        .filter(|line| !recognized.contains(line.trim()) && !by.is_match(line))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if motion_like(&other) {
                        warning(
                            warnings,
                            &path,
                            "Unaccounted motion-like VotingHTML text requires review",
                        );
                    }
                }
                if parsed == 0 && motion_like(&format!("{minutes} {html}")) {
                    warning(
                        warnings,
                        &path,
                        "Motion-like item without parsable named roll call",
                    );
                }
            }
        }
        if let Some(children) = item
            .get("ChildMinutesLst")
            .filter(|v| !v.is_null() && **v != false)
        {
            walk(
                children,
                &format!("{locator}.ChildMinutesLst"),
                motions,
                warnings,
            )?;
        }
    }
    Ok(())
}
pub fn parse(text: &str) -> Result<Value> {
    let preamble = "Public structured meeting records. JSON text; not a generated transcript. Source-reported votes do not establish formal minutes approval. Public projections retain their labels.";
    let body = if text.trim().starts_with('[') {
        text.trim()
    } else if let Some(body) = text.strip_prefix(preamble) {
        body.trim()
    } else {
        bail!("Unsupported Simbli capture envelope")
    };
    let documents: Value = serde_json::from_str(body)?;
    let documents = documents
        .as_array()
        .filter(|v| v.len() <= 2000)
        .context("Expected bounded capture document array")?;
    let mut motions = vec![];
    let mut warnings = vec![];
    let mut urls = vec![];
    let mut found = 0;
    for (i, d) in documents.iter().enumerate() {
        if d["document"]["role"] == "Minutes" {
            found += 1;
            if let Some(url) = d["document"]["source_url"].as_str() {
                if !urls.contains(&url) {
                    urls.push(url);
                }
            }
            walk(
                &d["public_content"]["LstItemMinutes"],
                &format!("documents[{i}].public_content.LstItemMinutes"),
                &mut motions,
                &mut warnings,
            )?;
        }
    }
    if found != 1 {
        bail!("Exactly one published Minutes document required");
    }
    if motions.len() > 500 {
        bail!("Maximum 500 parsed motions");
    }
    Ok(json!(
        { "adapter" : ADAPTER, "motions" : motions, "warnings" : warnings,
        "source_urls" : urls }
    ))
}
