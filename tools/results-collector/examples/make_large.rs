//! Build the reference 32 KiB county-item padding fixture one jurisdiction at a time.
use civpulse_results_source::{Expectations, Limits, prepare, stringify};
use std::{fs::File, io::Write, path::PathBuf, sync::atomic::AtomicBool};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        return Err("Usage: make_large INPUT OUTPUT".into());
    }
    let scratch = tempfile::tempdir()?;
    let prepared = prepare(
        &PathBuf::from(&args[0]),
        &scratch.path().join("scopes"),
        &Expectations {
            election_date: "2026-11-03".into(),
            expected_jurisdictions: Some(160),
            expected_items: Some(4178),
            accepted_topologies: vec![],
        },
        &Limits::default(),
        &AtomicBool::new(false),
    )?;
    let mut output = File::options()
        .write(true)
        .create_new(true)
        .open(&args[1])?;
    let header = stringify(&serde_json::Value::Object(prepared.header))?;
    output.write_all(&header[..header.len() - 1])?;
    for scope in &prepared.scopes {
        let mut value: serde_json::Value = serde_json::from_reader(File::open(&scope.path)?)?;
        if scope.index == 0 {
            output.write_all(b",\"results\":")?;
        } else {
            if scope.index == 1 {
                output.write_all(b",\"localResults\":[")?;
            } else {
                output.write_all(b",")?;
            }
            for item in value["ballotItems"]
                .as_array_mut()
                .ok_or("Missing ballot items")?
            {
                item["resourceTestPadding"] = serde_json::json!("x".repeat(32 * 1024));
            }
        }
        output.write_all(&stringify(&value)?)?;
    }
    output.write_all(b"]}")?;
    output.flush()?;
    Ok(())
}
