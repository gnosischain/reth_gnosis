pub mod download_init_state;
pub mod import_and_ensure_state;

// This is the first post-merge block on Gnosis Chain
// Currently ERA1 files are used only for pre-merge
pub const MAINNET_ERA_IMPORT_HEIGHT: u64 = 25_349_537; // Gnosis Chain
                                                       // pub const MAINNET_ERA_IMPORT_HEIGHT: u64 = 25_349_537; // Gnosis Chain

// REFERENCE MERGE BLOCKS:
// - Gnosis mainnet: block 25,349,537
// - Chiado testnet: block 680,930

pub const SNAPSHOT_API_URL: &str = "https://reth-snapshots.gnosischain.com";
