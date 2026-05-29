use alloy_primitives::Bytes;
use reth::version::{
    default_reth_version_metadata, try_init_version_metadata, RethCliVersionConsts,
};
use std::borrow::Cow;

/// reth_gnosis version, read from `Cargo.toml` at compile time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Full git commit SHA of this reth_gnosis build, set by `build.rs`.
/// Falls back to `"unknown"` when built outside a git checkout.
pub const GIT_SHA: &str = env!("RETH_GNOSIS_GIT_SHA");

/// `"true"` if the working tree was dirty at build time, else `"false"`.
const GIT_DIRTY: &str = env!("RETH_GNOSIS_GIT_DIRTY");

fn git_sha_short() -> &'static str {
    if GIT_SHA.len() >= 8 {
        &GIT_SHA[..8]
    } else {
        GIT_SHA
    }
}

fn dev_suffix() -> &'static str {
    if matches!(GIT_DIRTY, "true") {
        "-dev"
    } else {
        ""
    }
}

/// Install gnosis values into reth's global version metadata.
/// Must be called before CLI parsing
pub fn init_gnosis_version() {
    let upstream = default_reth_version_metadata();

    let target = upstream.vergen_cargo_target_triple.clone();
    let timestamp = upstream.vergen_build_timestamp.clone();
    let features = upstream.vergen_cargo_features.clone();
    let profile = upstream.build_profile_name.clone();
    let sha_short = git_sha_short();
    let dev = dev_suffix();

    let long_version = format!(
        "Version: {VERSION}{dev}\n\
         Commit SHA: {GIT_SHA}\n\
         Build Timestamp: {timestamp}\n\
         Build Features: {features}\n\
         Build Profile: {profile}",
    );

    if try_init_version_metadata(RethCliVersionConsts {
        name_client: Cow::Borrowed("reth_gnosis"),
        cargo_pkg_version: Cow::Borrowed(VERSION),
        vergen_git_sha_long: Cow::Borrowed(GIT_SHA),
        vergen_git_sha: Cow::Borrowed(sha_short),
        short_version: Cow::Owned(format!("{VERSION}{dev} ({sha_short})")),
        long_version: Cow::Owned(long_version),
        p2p_client_version: Cow::Owned(format!("reth_gnosis/v{VERSION}-{sha_short}/{target}")),
        extra_data: Cow::Owned(default_gnosis_extra_data()),
        ..upstream
    })
    .is_err()
    {
        eprintln!(
            "warning: reth version metadata was already initialized; \
             reth_gnosis values will not apply"
        );
    }
}

/// Default block `extra_data`: `reth_gnosis/v{version}`.
pub fn default_gnosis_extra_data() -> String {
    format!("reth_gnosis/v{VERSION}")
}

/// Default block `extra_data` as bytes.
pub fn default_gnosis_extra_data_bytes() -> Bytes {
    Bytes::from(default_gnosis_extra_data().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_data_under_32_bytes() {
        let extra = default_gnosis_extra_data();
        assert!(
            extra.len() <= 32,
            "extra_data must be <= 32 bytes, got {} ({extra:?})",
            extra.len()
        );
    }
}
