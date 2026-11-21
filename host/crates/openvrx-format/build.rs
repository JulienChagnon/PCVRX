// SPDX-License-Identifier: GPL-3.0-or-later
//! Puts the commit being built into OPENVRX_BUILD_COMMIT (recordings write
//! it into their sidecar's context). Without git or a checkout: "unknown".

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let commit = git(&["rev-parse", "--short=7", "HEAD"]).unwrap_or_else(|| "unknown".into());
    // Rebuild when HEAD moves: a commit, checkout or pull rewrites HEAD or
    // the ref it points to (refs/heads/... or packed-refs).
    // Only files that exist: Cargo reruns the script on every build for a
    // missing one, which would rebuild everything that depends on this crate.
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        let mut watch = vec![format!("{dir}/HEAD"), format!("{dir}/packed-refs")];
        if let Some(r) = git(&["symbolic-ref", "-q", "HEAD"]) {
            watch.push(format!("{dir}/{r}"));
        }
        for p in watch.iter().filter(|p| std::path::Path::new(p).exists()) {
            println!("cargo:rerun-if-changed={p}");
        }
    }
    println!("cargo:rustc-env=OPENVRX_BUILD_COMMIT={commit}");
}
