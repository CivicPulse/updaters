//! CI bridge: parse rendered chart caps using the same admission code as the worker.
use civpulse_results_collector::{Result, admission::Admission};
fn main() -> Result<()> {
    let caps = Admission::read(|key| std::env::var(key).ok())?;
    println!("{} {}", caps.download_bytes, caps.scope_bytes);
    Ok(())
}
