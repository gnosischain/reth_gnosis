//! Version information for reth_gnosis.

use alloy_primitives::Bytes;
use reth::version::{try_init_version_metadata, RethCliVersionConsts};
use std::borrow::Cow;

/// The upstream reth version this is based on
pub const RETH_UPSTREAM_VERSION: &str = env!("RETH_UPSTREAM_VERSION");

/// Initialize the global version metadata with reth_gnosis values.
/// MUST be called BEFORE any code calls `version_metadata()`.
pub fn init_gnosis_version() {
    let _ = try_init_version_metadata(gnosis_version_metadata());
}

/// Create the gnosis-specific version metadata
pub fn gnosis_version_metadata() -> RethCliVersionConsts {
    RethCliVersionConsts {
        name_client: Cow::Borrowed("reth_gnosis"),
        cargo_pkg_version: Cow::Borrowed(env!("CARGO_PKG_VERSION")),
        vergen_git_sha_long: Cow::Borrowed(env!("VERGEN_GIT_SHA")),
        vergen_git_sha: Cow::Borrowed(env!("VERGEN_GIT_SHA_SHORT")),
        vergen_build_timestamp: Cow::Borrowed(env!("VERGEN_BUILD_TIMESTAMP")),
        vergen_cargo_target_triple: Cow::Borrowed(env!("VERGEN_CARGO_TARGET_TRIPLE")),
        vergen_cargo_features: Cow::Borrowed(env!("VERGEN_CARGO_FEATURES")),
        short_version: Cow::Borrowed(env!("RETH_GNOSIS_SHORT_VERSION")),
        long_version: Cow::Owned(format!(
            "{}\n{}\n{}\n{}\n{}",
            env!("RETH_GNOSIS_LONG_VERSION_0"),
            env!("RETH_GNOSIS_LONG_VERSION_1"),
            env!("RETH_GNOSIS_LONG_VERSION_2"),
            env!("RETH_GNOSIS_LONG_VERSION_3"),
            env!("RETH_GNOSIS_LONG_VERSION_4"),
        )),
        build_profile_name: Cow::Borrowed(env!("RETH_GNOSIS_BUILD_PROFILE")),
        p2p_client_version: Cow::Borrowed(env!("RETH_GNOSIS_P2P_CLIENT_VERSION")),
        extra_data: Cow::Owned(default_gnosis_extra_data()),
    }
}

/// Default extra_data for built blocks: "reth_gnosis/v{version}"
pub fn default_gnosis_extra_data() -> String {
    format!("reth_gnosis/v{}", env!("CARGO_PKG_VERSION"))
}

/// Default extra_data as Bytes
pub fn default_gnosis_extra_data_bytes() -> Bytes {
    Bytes::from(default_gnosis_extra_data().as_bytes().to_vec())
}
