//! Build script for reth_gnosis - sets version info at compile time.
//! All env vars set here are available via env!() macros in src/ code.

use std::env;
use vergen::{BuildBuilder, CargoBuilder, Emitter};
use vergen_git2::Git2Builder;

/// The upstream reth version this build is based on.
/// Update this when bumping reth dependencies in Cargo.toml.
const RETH_UPSTREAM_VERSION: &str = "1.10.2";

fn main() {
    // Always set the upstream version
    println!("cargo:rustc-env=RETH_UPSTREAM_VERSION={RETH_UPSTREAM_VERSION}");

    // Try to get git/build info, fall back to defaults if unavailable
    if let Err(e) = try_set_version_info() {
        eprintln!("Warning: Could not get git info: {e}. Using fallback values.");
        set_fallback_version_info();
    }
}

fn try_set_version_info() -> Result<(), Box<dyn std::error::Error>> {
    let mut emitter = Emitter::default();

    let build = BuildBuilder::default().build_timestamp(true).build()?;
    emitter.add_instructions(&build)?;

    let cargo = CargoBuilder::default().features(true).target_triple(true).build()?;
    emitter.add_instructions(&cargo)?;

    let git = Git2Builder::default()
        .describe(false, true, None)
        .dirty(true)
        .sha(false)
        .build()?;
    emitter.add_instructions(&git)?;

    emitter.emit_and_set()?;

    let sha = env::var("VERGEN_GIT_SHA")?;
    let sha_short = &sha[0..8];

    let is_dirty = env::var("VERGEN_GIT_DIRTY")? == "true";
    // > git describe --always --tags
    // if not on a tag: v0.2.0-beta.3-82-g1939939b
    // if on a tag: v0.2.0-beta.3
    let not_on_tag = env::var("VERGEN_GIT_DESCRIBE")?.ends_with(&format!("-g{}", &sha[0..7]));
    let version_suffix = if is_dirty || not_on_tag { "-dev" } else { "" };

    // Set short SHA
    println!("cargo:rustc-env=VERGEN_GIT_SHA_SHORT={sha_short}");

    // Set the build profile
    let out_dir = env::var("OUT_DIR").unwrap();
    let profile = out_dir
        .rsplit(std::path::MAIN_SEPARATOR)
        .nth(3)
        .unwrap_or("unknown");
    println!("cargo:rustc-env=RETH_GNOSIS_BUILD_PROFILE={profile}");

    let pkg_version = env!("CARGO_PKG_VERSION");

    // Short version: "1.0.2-dev (abc12345)"
    let short_version = format!("{pkg_version}{version_suffix} ({sha_short})");
    println!("cargo:rustc-env=RETH_GNOSIS_SHORT_VERSION={short_version}");

    // Print version info to build logs for visibility
    println!("cargo:warning=Building reth_gnosis {short_version}");
    println!("cargo:warning=  Based on: reth {RETH_UPSTREAM_VERSION}");
    println!("cargo:warning=  Git SHA: {sha}");
    println!("cargo:warning=  Profile: {profile}");

    // Long version lines
    println!("cargo:rustc-env=RETH_GNOSIS_LONG_VERSION_0=Version: {pkg_version}{version_suffix}");
    println!("cargo:rustc-env=RETH_GNOSIS_LONG_VERSION_1=Commit SHA: {sha}");
    println!(
        "cargo:rustc-env=RETH_GNOSIS_LONG_VERSION_2=Build Timestamp: {}",
        env::var("VERGEN_BUILD_TIMESTAMP")?
    );
    println!(
        "cargo:rustc-env=RETH_GNOSIS_LONG_VERSION_3=Build Features: {}",
        env::var("VERGEN_CARGO_FEATURES")?
    );
    println!("cargo:rustc-env=RETH_GNOSIS_LONG_VERSION_4=Build Profile: {profile}");

    // P2P client version: "reth_gnosis/v1.0.2-abc12345/aarch64-apple-darwin"
    let target = env::var("VERGEN_CARGO_TARGET_TRIPLE")?;
    println!(
        "cargo:rustc-env=RETH_GNOSIS_P2P_CLIENT_VERSION=reth_gnosis/v{pkg_version}-{sha_short}/{target}"
    );

    Ok(())
}

fn set_fallback_version_info() {
    let pkg_version = env!("CARGO_PKG_VERSION");

    // Print fallback info to build logs
    println!("cargo:warning=Building reth_gnosis {pkg_version} (unknown) [fallback - no git info]");
    println!("cargo:warning=  Based on: reth {RETH_UPSTREAM_VERSION}");

    println!("cargo:rustc-env=VERGEN_GIT_SHA=unknown");
    println!("cargo:rustc-env=VERGEN_GIT_SHA_SHORT=unknown");
    println!("cargo:rustc-env=VERGEN_BUILD_TIMESTAMP=unknown");
    println!("cargo:rustc-env=VERGEN_CARGO_TARGET_TRIPLE=unknown");
    println!("cargo:rustc-env=VERGEN_CARGO_FEATURES=unknown");
    println!("cargo:rustc-env=RETH_GNOSIS_BUILD_PROFILE=unknown");
    println!("cargo:rustc-env=RETH_GNOSIS_SHORT_VERSION={pkg_version} (unknown)");
    println!("cargo:rustc-env=RETH_GNOSIS_LONG_VERSION_0=Version: {pkg_version}");
    println!("cargo:rustc-env=RETH_GNOSIS_LONG_VERSION_1=Commit SHA: unknown");
    println!("cargo:rustc-env=RETH_GNOSIS_LONG_VERSION_2=Build Timestamp: unknown");
    println!("cargo:rustc-env=RETH_GNOSIS_LONG_VERSION_3=Build Features: unknown");
    println!("cargo:rustc-env=RETH_GNOSIS_LONG_VERSION_4=Build Profile: unknown");
    println!("cargo:rustc-env=RETH_GNOSIS_P2P_CLIENT_VERSION=reth_gnosis/v{pkg_version}-unknown/unknown");
}
