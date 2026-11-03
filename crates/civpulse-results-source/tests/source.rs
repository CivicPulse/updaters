use std::{fs, path::PathBuf, sync::atomic::AtomicBool};

use civpulse_results_source::{Expectations, Limits, canonical, prepare, stringify};
use serde_json::{Value, json};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../civpulse-results-contract/tests/fixtures")
        .join(name)
}
fn expectations() -> Expectations {
    Expectations {
        election_date: "2026-11-03".to_owned(),
        expected_jurisdictions: Some(2),
        expected_items: Some(2),
    }
}
fn run(bytes: &[u8], limits: &Limits, expected: &Expectations) -> bool {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("source.json");
    fs::write(&input, bytes).unwrap();
    let output = temp.path().join("spool");
    let result = prepare(&input, &output, expected, limits, &AtomicBool::new(false));
    if result.is_err() {
        assert!(!output.exists(), "failed preparation must clean spool");
    }
    result.is_ok()
}
fn base() -> Value {
    serde_json::from_slice(&fs::read(fixture("base.json")).unwrap()).unwrap()
}

#[test]
fn actual_rust_parser_and_fingerprints_match_upstream_goldens() {
    let expected: Value =
        serde_json::from_slice(&fs::read(fixture("expected.json")).unwrap()).unwrap();
    for (name, case) in expected["cases"].as_object().unwrap() {
        let temp = tempfile::tempdir().unwrap();
        let prepared = prepare(
            &fixture(&format!("{name}.json")),
            &temp.path().join("scopes"),
            &expectations(),
            &Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(prepared.fingerprint, case["fingerprint"], "{name}");
        assert_eq!(prepared.item_count, 2);
        assert_eq!(json!(prepared.jurisdictions), case["jurisdictions"]);
        let scope: Value =
            serde_json::from_slice(&fs::read(&prepared.scopes[0].path).unwrap()).unwrap();
        assert_eq!(
            String::from_utf8(canonical(&scope).unwrap()).unwrap(),
            case["canonical_scope"],
            "{name}"
        );
    }
}

#[test]
fn stringify_matches_javascript_numbers_escaping_and_index_properties() {
    let input = r#"{"z":-0.0,"10":"ten","2":"two","e":[1e-7,1e-6,1e20,1e21],"s":"é😀\n\t\u0000"}"#;
    let value: Value = serde_json::from_str(input).unwrap();
    assert_eq!(
        String::from_utf8(stringify(&value).unwrap()).unwrap(),
        "{\"2\":\"two\",\"10\":\"ten\",\"z\":0,\"e\":[1e-7,0.000001,100000000000000000000,1e+21],\"s\":\"é😀\\n\\t\\u0000\"}"
    );
}

#[test]
fn metadata_cap_measures_javascript_bytes_before_derived_item_count() {
    let mut value = base();
    value["results"]["sourceNumericMetadata"] = json!(vec![1e20; 7_000]);
    let mut metadata = value["results"].clone();
    metadata.as_object_mut().unwrap().remove("ballotItems");
    assert_eq!(stringify(&metadata).unwrap().len(), 154_080);
    assert!(serde_json::to_vec(&metadata).unwrap().len() < Limits::default().max_metadata);
    assert!(!run(
        &serde_json::to_vec(&value).unwrap(),
        &Limits::default(),
        &expectations()
    ));

    // Exactly 128 KiB of source metadata is valid even though adding the derived
    // item_count makes the eventual jurisdiction manifest entry larger.
    let mut value = base();
    value["results"]["sourcePadding"] = json!("");
    let mut metadata = value["results"].clone();
    metadata.as_object_mut().unwrap().remove("ballotItems");
    let capacity = Limits::default().max_metadata - stringify(&metadata).unwrap().len();
    value["results"]["sourcePadding"] = json!("x".repeat(capacity));
    metadata["sourcePadding"] = value["results"]["sourcePadding"].clone();
    assert_eq!(
        stringify(&metadata).unwrap().len(),
        Limits::default().max_metadata
    );
    assert!(run(
        &serde_json::to_vec(&value).unwrap(),
        &Limits::default(),
        &expectations()
    ));
    value["results"]["sourcePadding"] = json!("x".repeat(capacity + 1));
    assert!(!run(
        &serde_json::to_vec(&value).unwrap(),
        &Limits::default(),
        &expectations()
    ));
}

#[test]
fn header_cap_measures_javascript_bytes_and_accepts_the_exact_boundary() {
    let mut value = base();
    value["electionName"] = json!("");
    let mut header = value.clone();
    header.as_object_mut().unwrap().remove("results");
    header.as_object_mut().unwrap().remove("localResults");
    let capacity = Limits::default().max_header - stringify(&header).unwrap().len();
    value["electionName"] = json!("x".repeat(capacity));
    assert!(run(
        &serde_json::to_vec(&value).unwrap(),
        &Limits::default(),
        &expectations()
    ));
    value["electionName"] = json!("x".repeat(capacity + 1));
    assert!(!run(
        &serde_json::to_vec(&value).unwrap(),
        &Limits::default(),
        &expectations()
    ));

    // Header fields must ultimately be strings, but their size check runs before
    // identity validation. Numeric expansion must fail the bound at that stage.
    value["electionName"] = json!(vec![1e20; 50_000]);
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("source.json");
    fs::write(&input, serde_json::to_vec(&value).unwrap()).unwrap();
    let error = prepare(
        &input,
        &temp.path().join("scopes"),
        &expectations(),
        &Limits::default(),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        civpulse_results_source::Error::Invalid("feed header exceeds limit")
    ));
}

#[test]
fn invalid_schema_duplicates_counts_and_completeness_fail_closed() {
    let limits = Limits::default();
    let mutate: Vec<fn(&mut Value)> = vec![
        (|v| v["electionDate"] = json!("2026-11-04")),
        (|v| v["createdAt"] = json!("2999-11-03T00:00:00Z")),
        (|v| v["localResults"][0]["id"] = v["results"]["id"].clone()),
        (|v| v["results"]["ballotItems"][0]["ballotOptions"][1]["id"] = json!("A")),
        (|v| v["results"]["ballotItems"][0]["ballotOptions"][0]["voteCount"] = json!(-1)),
        (|v| v["results"]["ballotItems"][0]["ballotOptions"][0]["voteCount"] = json!(1.5)),
        (|v| {
            v["results"]["ballotItems"][0]["ballotOptions"][0]["voteCount"] =
                json!(9_007_199_254_740_992_u64);
        }),
        (|v| v["results"]["ballotItems"][0]["ballotOptions"][0]["voteCount"] = json!("12")),
        (|v| v["unexpectedSchema"] = json!(1)),
        (|v| v["results"]["reportingStatuses"] = json!({})),
        (|v| {
            v["results"]["ballotItems"][0]["ballotOptions"][0]["precinctResults"] = json!({});
        }),
    ];
    for mutate in mutate {
        let mut value = base();
        mutate(&mut value);
        assert!(!run(
            &serde_json::to_vec(&value).unwrap(),
            &limits,
            &expectations()
        ));
    }
    let bytes = fs::read(fixture("base.json")).unwrap();
    assert!(!run(&bytes[..bytes.len() - 1], &limits, &expectations()));
    let duplicate = String::from_utf8(bytes.clone())
        .unwrap()
        .replace("\"voteCount\":12", "\"voteCount\":12,\"voteCount\":13");
    assert!(!run(duplicate.as_bytes(), &limits, &expectations()));
    let duplicate_root = String::from_utf8(bytes.clone()).unwrap().replacen(
        '{',
        "{\"electionDate\":\"2026-11-03\",",
        1,
    );
    assert!(!run(duplicate_root.as_bytes(), &limits, &expectations()));
    assert!(!run(
        &bytes,
        &limits,
        &Expectations {
            expected_items: Some(3),
            ..expectations()
        }
    ));
    assert!(!run(
        &bytes,
        &Limits {
            max_download: 2,
            ..limits.clone()
        },
        &expectations()
    ));
    assert!(!run(
        &bytes,
        &Limits {
            max_scope: 10,
            ..limits.clone()
        },
        &expectations()
    ));
    assert!(!run(
        &bytes,
        &Limits {
            max_header: 10,
            ..limits.clone()
        },
        &expectations()
    ));
    assert!(!run(
        &bytes,
        &Limits {
            max_metadata: 10,
            ..limits.clone()
        },
        &expectations()
    ));
    assert!(!run(
        &bytes,
        &Limits {
            max_batch_bytes: 10,
            ..limits.clone()
        },
        &expectations()
    ));
    assert!(!run(
        &bytes,
        &Limits {
            max_jurisdictions: 1,
            ..limits
        },
        &expectations()
    ));
}

#[test]
fn source_order_and_nullable_evidence_survive_and_cancel_is_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let mut value = base();
    let object = value.as_object_mut().unwrap();
    let state = object.remove("results").unwrap();
    object.insert("results".to_owned(), state);
    assert!(run(
        &serde_json::to_vec(&value).unwrap(),
        &Limits::default(),
        &expectations()
    ));
    let output = temp.path().join("cancelled");
    assert!(
        prepare(
            &fixture("base.json"),
            &output,
            &expectations(),
            &Limits::default(),
            &AtomicBool::new(true)
        )
        .is_err()
    );
    assert!(!output.exists());
}

#[test]
fn excessive_nesting_nested_id_duplicates_and_deadlines_fail_closed() {
    let mut value = base();
    let option = &mut value["results"]["ballotItems"][0]["ballotOptions"][0];
    option["precinctResults"] = json!([{"id":"p1","voteCount":1},{"id":"p1","voteCount":2}]);
    assert!(!run(
        &serde_json::to_vec(&value).unwrap(),
        &Limits::default(),
        &expectations()
    ));
    let mut value = base();
    let mut nested = Value::Null;
    for _ in 0..41 {
        nested = json!([nested]);
    }
    value["results"]["nested"] = nested;
    assert!(!run(
        &serde_json::to_vec(&value).unwrap(),
        &Limits::default(),
        &expectations()
    ));
    assert!(!run(
        &fs::read(fixture("base.json")).unwrap(),
        &Limits {
            parse_timeout: std::time::Duration::ZERO,
            ..Limits::default()
        },
        &expectations()
    ));
}

#[test]
fn empty_precinct_source_ids_are_preserved_as_official_evidence() {
    let mut value = base();
    value["results"]["ballotItems"][0]["ballotOptions"][0]["precinctResults"][0]["id"] = json!("");
    assert!(run(
        &serde_json::to_vec(&value).unwrap(),
        &Limits::default(),
        &expectations()
    ));
}

#[test]
fn local_sources_must_be_regular_files() {
    let temp = tempfile::tempdir().unwrap();
    assert!(
        prepare(
            temp.path(),
            &temp.path().join("spool"),
            &expectations(),
            &Limits::default(),
            &AtomicBool::new(false)
        )
        .is_err()
    );
    #[cfg(unix)]
    assert!(
        prepare(
            std::path::Path::new("/dev/null"),
            &temp.path().join("device-spool"),
            &expectations(),
            &Limits::default(),
            &AtomicBool::new(false)
        )
        .is_err()
    );
}

#[test]
fn unknown_root_header_numbers_remain_rejected_before_publication() {
    let mut source = base();
    source["sourceSequence"] = json!(9_007_199_254_740_993_u64);
    assert!(!run(
        &serde_json::to_vec(&source).unwrap(),
        &Limits::default(),
        &expectations()
    ));
}
