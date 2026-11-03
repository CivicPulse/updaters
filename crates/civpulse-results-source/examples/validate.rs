//! Local validation/resource measurement: no credentials or external writes.
use civpulse_results_source::{Expectations, Limits, prepare};
use std::{path::Path, sync::atomic::AtomicBool, time::Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = std::env::args()
        .nth(1)
        .ok_or("usage: validate <source.json>")?;
    let temp = tempfile::tempdir()?;
    let start = Instant::now();
    let prepared = prepare(
        Path::new(&input),
        &temp.path().join("scopes"),
        &Expectations {
            election_date: "2026-11-03".to_owned(),
            expected_jurisdictions: Some(160),
            expected_items: Some(4_178),
        },
        &Limits::default(),
        &AtomicBool::new(false),
    )?;
    println!(
        "jurisdictions={} items={} fingerprint={} duration_ms={}",
        prepared.scopes.len(),
        prepared.item_count,
        prepared.fingerprint,
        start.elapsed().as_millis()
    );
    Ok(())
}
