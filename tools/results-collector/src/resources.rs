//! Read-only measurements from this process and its own container cgroup.
use crate::{Error, Result};
use serde::Serialize;

#[derive(Default, Serialize)]
pub struct Resources {
    pub cgroup_memory_current_bytes: Option<u64>,
    pub cgroup_memory_peak_bytes: Option<u64>,
    pub cgroup_memory_limit_bytes: Option<u64>,
    pub cgroup_memory_unlimited: Option<bool>,
    pub cgroup_cpu_usage_usec: Option<u64>,
    pub cgroup_cpu_user_usec: Option<u64>,
    pub cgroup_cpu_system_usec: Option<u64>,
    pub cgroup_cpu_throttled_usec: Option<u64>,
    pub cgroup_cpu_throttled_periods: Option<u64>,
    pub cgroup_memory_anon_bytes: Option<u64>,
    pub cgroup_memory_file_bytes: Option<u64>,
    pub cgroup_memory_shmem_bytes: Option<u64>,
    pub cgroup_memory_oom_events: Option<u64>,
    pub cgroup_memory_oom_kill_events: Option<u64>,
    pub scratch_spool_logical_bytes: Option<u64>,
    pub scratch_spool_allocated_bytes: Option<u64>,
    pub process_rss_bytes: Option<u64>,
    pub process_peak_rss_bytes: Option<u64>,
}
/// Read only fixed kernel-owned paths; unsupported counters remain JSON null.
/// The process RSS is this invocation's RSS; cgroup counters include the daemon
/// and the small measurement invocation. Cgroup peak is lifetime high-water, not
/// an invented per-job peak. No Metrics Server, shell, credentials or network is used.
#[must_use]
pub fn read() -> Resources {
    let owned = std::fs::read_to_string("/proc/self/cgroup")
        .ok()
        .and_then(|text| owned_cgroup(&text));
    let file = |name| {
        owned
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path.join(name)).ok())
    };
    let mut sample = parse(
        file("memory.current").as_deref(),
        file("memory.peak").as_deref(),
        file("memory.max").as_deref(),
        file("cpu.stat").as_deref(),
        std::fs::read_to_string("/proc/self/status").ok().as_deref(),
    );
    let stat = file("memory.stat");
    let events = file("memory.events");
    sample.cgroup_memory_anon_bytes = named_counter(stat.as_deref(), "anon");
    sample.cgroup_memory_file_bytes = named_counter(stat.as_deref(), "file");
    sample.cgroup_memory_shmem_bytes = named_counter(stat.as_deref(), "shmem");
    sample.cgroup_memory_oom_events = named_counter(events.as_deref(), "oom");
    sample.cgroup_memory_oom_kill_events = named_counter(events.as_deref(), "oom_kill");
    if let Some((logical, allocated)) = scratch() {
        sample.scratch_spool_logical_bytes = Some(logical);
        sample.scratch_spool_allocated_bytes = allocated;
    }
    sample
}
fn owned_cgroup(text: &str) -> Option<std::path::PathBuf> {
    use std::path::{Component, Path};
    let group = text.lines().find_map(|line| line.strip_prefix("0::"))?;
    if !group.starts_with('/') || group.len() > 4096 {
        return None;
    }
    let mut root = std::path::PathBuf::from("/sys/fs/cgroup");
    for component in Path::new(group).components() {
        match component {
            Component::RootDir => {}
            Component::Normal(value) => root.push(value),
            _ => return None,
        }
    }
    Some(root)
}

fn parse(
    current: Option<&str>,
    peak: Option<&str>,
    limit: Option<&str>,
    cpu: Option<&str>,
    process: Option<&str>,
) -> Resources {
    let number = |value: Option<&str>| value.and_then(|value| value.trim().parse::<u64>().ok());
    let counter = |name| {
        cpu.and_then(|text| {
            text.lines().find_map(|line| {
                let mut words = line.split_whitespace();
                (words.next() == Some(name))
                    .then(|| words.next().and_then(|value| value.parse().ok()))
                    .flatten()
            })
        })
    };
    let rss = |name| {
        process.and_then(|text| {
            text.lines().find_map(|line| {
                let (field, value) = line.split_once(':')?;
                if field != name {
                    return None;
                }
                let mut words = value.split_whitespace();
                let n = words.next()?.parse::<u64>().ok()?;
                if words.next() != Some("kB") {
                    return None;
                }
                n.checked_mul(1024)
            })
        })
    };
    let unlimited = limit.and_then(|value| {
        if value.trim() == "max" {
            Some(true)
        } else {
            number(Some(value)).map(|_| false)
        }
    });
    Resources {
        cgroup_memory_current_bytes: number(current),
        cgroup_memory_peak_bytes: number(peak),
        cgroup_memory_limit_bytes: number(limit),
        cgroup_memory_unlimited: unlimited,
        cgroup_cpu_usage_usec: counter("usage_usec"),
        cgroup_cpu_user_usec: counter("user_usec"),
        cgroup_cpu_system_usec: counter("system_usec"),
        cgroup_cpu_throttled_usec: counter("throttled_usec"),
        cgroup_cpu_throttled_periods: counter("nr_throttled"),
        process_rss_bytes: rss("VmRSS"),
        process_peak_rss_bytes: rss("VmHWM"),
        ..Resources::default()
    }
}
fn named_counter(text: Option<&str>, name: &str) -> Option<u64> {
    text.and_then(|text| {
        text.lines().find_map(|line| {
            let mut words = line.split_whitespace();
            (words.next() == Some(name))
                .then(|| words.next().and_then(|n| n.parse().ok()))
                .flatten()
        })
    })
}
fn scratch() -> Option<(u64, Option<u64>)> {
    let mut logical = 0_u64;
    let mut allocated = Some(0_u64);
    let mut remaining = 4096;
    for entry in std::fs::read_dir("/tmp").ok()? {
        if remaining == 0 {
            return None;
        }
        remaining -= 1;
        let entry = entry.ok()?;
        if !entry.file_name().to_str()?.starts_with("civpulse-results-") {
            continue;
        }
        let metadata = std::fs::symlink_metadata(entry.path()).ok()?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        walk_spool(
            &entry.path(),
            0,
            &mut remaining,
            &mut logical,
            &mut allocated,
        )?;
    }
    Some((logical, allocated))
}
fn walk_spool(
    path: &std::path::Path,
    depth: usize,
    remaining: &mut usize,
    logical: &mut u64,
    allocated: &mut Option<u64>,
) -> Option<()> {
    if depth > 4 || *remaining == 0 {
        return None;
    }
    *remaining -= 1;
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(()),
        Err(_) => return None,
    };
    if metadata.file_type().is_symlink() {
        return Some(());
    }
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path).ok()? {
            walk_spool(
                &entry.ok()?.path(),
                depth + 1,
                remaining,
                logical,
                allocated,
            )?;
        }
    } else if metadata.is_file() {
        *logical = logical.checked_add(metadata.len())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            *allocated = allocated.and_then(|n| {
                metadata
                    .blocks()
                    .checked_mul(512)
                    .and_then(|bytes| n.checked_add(bytes))
            });
        }
        #[cfg(not(unix))]
        {
            *allocated = None;
        }
    }
    Some(())
}
/// Print the current container/process counters without reading operational secrets.
/// # Errors
/// Returns only serialization failures.
pub fn command() -> Result<()> {
    println!(
        "{}",
        serde_json::to_string(&read()).map_err(|_| Error::InvalidResponse)?
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kernel_counters_and_kib_units_are_reported_exactly() {
        let r = parse(
            Some("1024\n"),
            Some("2048\n"),
            Some("1073741824\n"),
            Some(
                "usage_usec 100\nuser_usec 75\nsystem_usec 25\nnr_throttled 2\nthrottled_usec 50\n",
            ),
            Some("Name:\tcollector\nVmRSS:\t123 kB\nVmHWM:\t234 kB\n"),
        );
        assert_eq!(r.cgroup_memory_current_bytes, Some(1024));
        assert_eq!(r.cgroup_memory_peak_bytes, Some(2048));
        assert_eq!(r.cgroup_memory_limit_bytes, Some(1_073_741_824));
        assert_eq!(r.cgroup_memory_unlimited, Some(false));
        assert_eq!(r.cgroup_cpu_usage_usec, Some(100));
        assert_eq!(r.process_rss_bytes, Some(123 * 1024));
        assert_eq!(r.process_peak_rss_bytes, Some(234 * 1024));
    }
    #[test]
    fn unsupported_unlimited_and_real_zero_are_distinct() {
        let unsupported = parse(None, None, None, None, None);
        assert!(
            serde_json::to_value(unsupported)
                .unwrap()
                .as_object()
                .unwrap()
                .values()
                .all(serde_json::Value::is_null)
        );
        let unlimited = parse(
            Some("0"),
            Some("bad"),
            Some("max"),
            Some("usage_usec 0\n"),
            Some("VmRSS:\t999999999999999999999999 kB\n"),
        );
        assert_eq!(unlimited.cgroup_memory_current_bytes, Some(0));
        assert_eq!(unlimited.cgroup_memory_peak_bytes, None);
        assert_eq!(unlimited.cgroup_memory_limit_bytes, None);
        assert_eq!(unlimited.cgroup_memory_unlimited, Some(true));
        assert_eq!(unlimited.cgroup_cpu_usage_usec, Some(0));
        assert_eq!(unlimited.process_rss_bytes, None);
    }
    #[test]
    fn only_kernel_reported_cgroup_paths_inside_the_fixed_mount_are_read() {
        assert_eq!(
            owned_cgroup("0::/\n").unwrap(),
            std::path::PathBuf::from("/sys/fs/cgroup")
        );
        assert_eq!(
            owned_cgroup("0::/pod/collector\n").unwrap(),
            std::path::PathBuf::from("/sys/fs/cgroup/pod/collector")
        );
        assert!(owned_cgroup("0::/../../etc\n").is_none());
        assert!(owned_cgroup("2:memory:/pod\n").is_none());
    }
    #[test]
    fn page_cache_and_oom_counters_are_not_inferred_from_process_rss() {
        assert_eq!(
            named_counter(Some("anon 100\nfile 200\nshmem 30\n"), "file"),
            Some(200)
        );
        assert_eq!(
            named_counter(Some("oom 2\noom_kill 1\n"), "oom_kill"),
            Some(1)
        );
        assert_eq!(named_counter(None, "anon"), None);
    }
    #[test]
    fn scratch_walk_counts_real_files_and_refuses_unbounded_or_symlinked_work() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("raw.json");
        std::fs::write(&path, b"evidence").unwrap();
        let mut logical = 0;
        let mut allocated = Some(0);
        let mut remaining = 10;
        walk_spool(dir.path(), 0, &mut remaining, &mut logical, &mut allocated).unwrap();
        assert_eq!(logical, 8);
        assert!(walk_spool(dir.path(), 0, &mut 0, &mut 0, &mut Some(0)).is_none());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/passwd", dir.path().join("outside")).unwrap();
            let mut logical = 0;
            walk_spool(dir.path(), 0, &mut 10, &mut logical, &mut Some(0)).unwrap();
            assert_eq!(logical, 8);
        }
    }
}
