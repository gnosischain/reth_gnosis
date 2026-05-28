//! Emits reth_gnosis-specific git info as compile-time env vars for use via
//! `env!` macros in `src/version.rs`. Shells out to `git` directly to avoid
//! pulling in `vergen` / `vergen-git2`.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");

    let sha = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&["status", "--porcelain"])
        .map(|out| !out.is_empty())
        .unwrap_or(false);

    println!("cargo:rustc-env=RETH_GNOSIS_GIT_SHA={sha}");
    println!(
        "cargo:rustc-env=RETH_GNOSIS_GIT_DIRTY={}",
        if dirty { "true" } else { "false" }
    );
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
