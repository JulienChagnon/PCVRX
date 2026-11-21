// SPDX-License-Identifier: GPL-3.0-or-later
//! The sidecar's `context` block: what a recording program knew besides the
//! signal. [`host_context`] gives the keys every recorder shares (build,
//! host, command line); programs add their own (the app: `app`, `summary`).
//!
//! Everything here is best effort: a value the platform does not offer is
//! left out rather than guessed, and nothing here can fail a recording.

use serde_json::{json, Map, Value};

/// Commit the binary was built from (short hash of HEAD at build time), or
/// "unknown" outside a git checkout. Uncommitted changes are not shown: the
/// executable's time (`exe_modified_utc`) tells a local build apart.
pub const BUILD_COMMIT: &str = env!("OPENVRX_BUILD_COMMIT");

/// `build`, `host` and `command_line`, for a sidecar's `context`.
pub fn host_context() -> Map<String, Value> {
    let mut c = Map::new();
    c.insert("build".into(), build());
    c.insert("host".into(), host());
    c.insert("command_line".into(), json!(std::env::args().collect::<Vec<_>>()));
    c
}

fn build() -> Value {
    let exe = std::env::current_exe().ok();
    let exe_modified = exe
        .as_ref()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| time::OffsetDateTime::from(t).format(&time::format_description::well_known::Rfc3339).ok());
    json!({
        "software": crate::software_string(),
        "commit": BUILD_COMMIT,
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "exe": exe.map(|p| p.display().to_string()),
        "exe_modified_utc": exe_modified,
    })
}

fn host() -> Value {
    let mut h = Map::new();
    h.insert("os".into(), json!(std::env::consts::OS));
    h.insert("arch".into(), json!(std::env::consts::ARCH));
    if let Ok(n) = std::thread::available_parallelism() {
        h.insert("cpus".into(), json!(n.get()));
    }
    if let Some(name) = hostname() {
        h.insert("name".into(), json!(name));
    }
    #[cfg(target_os = "linux")]
    linux_host(&mut h);
    if let Value::Object(load) = host_load() {
        h.extend(load);
    }
    Value::Object(h)
}

fn hostname() -> Option<String> {
    #[cfg(target_os = "linux")]
    if let Some(n) = read_trim("/proc/sys/kernel/hostname") {
        return Some(n);
    }
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).ok()
}

#[cfg(target_os = "linux")]
fn linux_host(h: &mut Map<String, Value>) {
    if let Some(k) = read_trim("/proc/sys/kernel/osrelease") {
        h.insert("kernel".into(), json!(k));
    }
    if let Some(model) = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|t| t.lines().find(|l| l.starts_with("model name")).map(|l| l.to_string()))
        .and_then(|l| l.split_once(':').map(|(_, v)| v.trim().to_string()))
    {
        h.insert("cpu".into(), json!(model));
    }
    if let Some(g) = read_trim("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor") {
        h.insert("cpu_governor".into(), json!(g));
    }
    if let Some(kb) = meminfo_kb("MemTotal:") {
        h.insert("mem_total_mb".into(), json!(kb / 1024));
    }
}

/// The host's load right now: load average and free memory (Linux), so a
/// recording shows when the PC was busy with something else. Logged at the
/// start, then once a second by the app.
pub fn host_load() -> Value {
    let mut m = Map::new();
    #[cfg(target_os = "linux")]
    {
        if let Some(l) = read_trim("/proc/loadavg") {
            let v: Vec<f64> = l.split_whitespace().take(3).filter_map(|x| x.parse().ok()).collect();
            if v.len() == 3 {
                m.insert("load_1m".into(), json!(v[0]));
                m.insert("load_5m".into(), json!(v[1]));
            }
        }
        if let Some(kb) = meminfo_kb("MemAvailable:") {
            m.insert("mem_available_mb".into(), json!(kb / 1024));
        }
    }
    Value::Object(m)
}

/// The file system a path is on, as the kernel names it ("ntfs3",
/// "fuseblk", "btrfs"...): the longest mount point that contains it (Linux).
pub fn fs_type(path: &std::path::Path) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let p = std::fs::canonicalize(path).ok()?;
        let mounts = std::fs::read_to_string("/proc/self/mounts").ok()?;
        let mut best: Option<(usize, String)> = None;
        for line in mounts.lines() {
            let mut f = line.split_whitespace();
            let (Some(_dev), Some(mnt), Some(fs)) = (f.next(), f.next(), f.next()) else { continue };
            // Mount points escape spaces as \040.
            let mnt = mnt.replace("\\040", " ");
            if p.starts_with(&mnt) && best.as_ref().is_none_or(|b| mnt.len() >= b.0) {
                best = Some((mnt.len(), fs.to_string()));
            }
        }
        best.map(|b| b.1)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        None
    }
}

#[cfg(target_os = "linux")]
fn read_trim(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

#[cfg(target_os = "linux")]
fn meminfo_kb(key: &str) -> Option<u64> {
    let t = std::fs::read_to_string("/proc/meminfo").ok()?;
    t.lines()
        .find(|l| l.starts_with(key))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_the_shared_keys() {
        let c = host_context();
        assert!(c["build"]["software"].as_str().unwrap().starts_with("openvrx "));
        assert!(!c["build"]["commit"].as_str().unwrap().is_empty());
        assert_eq!(c["host"]["os"], std::env::consts::OS);
        assert!(c["command_line"].as_array().is_some_and(|a| !a.is_empty()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_extras() {
        let c = host_context();
        assert!(c["host"]["load_1m"].is_number());
        assert!(c["host"]["mem_total_mb"].as_u64().unwrap() > 0);
        assert!(fs_type(std::path::Path::new("/proc")).as_deref() == Some("proc"));
        assert!(fs_type(std::path::Path::new("/")).is_some());
    }
}
