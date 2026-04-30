pub mod download_init_state;
pub mod import_and_ensure_state;

// This is the post-merge block height on Gnosis Chain at which our state file is created
// TODO: restore to 26478650 for full sync
pub const MAINNET_ERA_IMPORT_HEIGHT: u64 = 13_300_000; // Gnosis Chain

// MERGE BLOCKS:
// - Gnosis mainnet: block 25,349,537
// - Chiado testnet: block 680,930
