use civpulse_results_contract::{Pointer, Revision, ScopeAsset};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf};

fn fixture(name: &str) -> Vec<u8> {
    fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .expect("golden fixture")
}

fn expected() -> Value {
    serde_json::from_slice(&fixture("expected.json")).unwrap()
}

#[test]
fn oracle_receipts_and_assets_are_exact_bytes() {
    let expected = expected();
    assert_eq!(
        hex::encode(Sha256::digest(fixture("canonical-scope.json"))),
        expected["canonical_scope_sha256"]
    );
    for (name, case) in expected["cases"].as_object().unwrap() {
        let hash = hex::encode(Sha256::digest(fixture(&format!("{name}.json"))));
        assert_eq!(hash, case["raw_sha256"]);
        assert_eq!(case["item_count"], 2);
    }
    for asset in expected["assets"].as_array().unwrap() {
        let hash = hex::encode(Sha256::digest(fixture(asset["file"].as_str().unwrap())));
        assert_eq!(hash, asset["sha256"]);
    }
}

#[test]
fn oracle_fingerprints_distinguish_meaningful_changes() {
    let cases = &expected()["cases"];
    assert_eq!(
        cases["base"]["fingerprint"],
        cases["reordered"]["fingerprint"]
    );
    assert_ne!(
        cases["base"]["raw_sha256"],
        cases["reordered"]["raw_sha256"]
    );
    for changed in ["correction", "label", "status"] {
        assert_ne!(cases["base"]["fingerprint"], cases[changed]["fingerprint"]);
    }
}

#[test]
fn pointer_and_scope_round_trip_without_losing_source_evidence() {
    let bytes = fixture("latest.json");
    let pointer: Pointer = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        serde_json::to_value(&pointer).unwrap(),
        serde_json::from_slice::<Value>(&bytes).unwrap()
    );
    assert_eq!(pointer.jurisdictions[0].id, "GA");
    for file in [
        "assets/state.json",
        "assets/state-detail.json",
        "assets/counties/001.json",
        "assets/counties/001-detail.json",
    ] {
        let bytes = fixture(file);
        let asset: ScopeAsset = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            serde_json::to_value(&asset).unwrap(),
            serde_json::from_slice::<Value>(&bytes).unwrap()
        );
        let options = asset.items[0].data["ballotOptions"].as_array().unwrap();
        assert_eq!(options[0]["voteCount"], 12);
        assert_eq!(options[1]["voteCount"], 0);
        assert_eq!(options[2].get("voteCount"), Some(&Value::Null));
        assert!(options[3].get("voteCount").is_none());
        assert!(asset.jurisdiction["reportingStatuses"].is_null());
        assert_eq!(
            options[0].get("precinctResults").is_some(),
            file.contains("detail")
        );
        assert_eq!(
            asset.items[0].data.contains_key("rankedChoiceResults"),
            file.contains("detail")
        );
    }
}

#[test]
fn revisions_do_not_use_float_or_lexicographic_ordering() {
    let revision = |text: &str| serde_json::from_value::<Revision>(json!(text)).unwrap();
    assert!(revision("9007199254740993") > revision("9007199254740992"));
    assert!(revision("100") > revision("99"));
    assert_eq!(revision("00042"), revision("42"));
    assert_eq!(
        serde_json::to_value(revision("9007199254740993")).unwrap(),
        json!("9007199254740993")
    );
    for invalid in [
        json!(9_007_199_254_740_993_u64),
        json!(""),
        json!("-1"),
        json!("1.0"),
        json!(" 1"),
    ] {
        assert!(serde_json::from_value::<Revision>(invalid).is_err());
    }
}
