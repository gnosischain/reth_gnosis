pub mod download_init_state;
pub mod import_and_ensure_state;

// This is the post-merge block height on Gnosis Chain and Chiado at which our state file is created
pub const MAINNET_ERA_IMPORT_HEIGHT: u64 = 26_478_650; // Gnosis Chain
pub const CHIADO_ERA_IMPORT_HEIGHT: u64 = 700_000; // Chiado
