use std::sync::Arc;

use reth_evm::execute::BasicBlockExecutorProvider;

use crate::{evm_config::GnosisEvmConfig, spec::GnosisChainSpec};

// REF: https://github.com/paradigmxyz/reth/blob/d3b299754fe79b051bec022e67e922f6792f2a17/crates/ethereum/evm/src/execute.rs
/// Helper type with backwards compatible methods to obtain executor providers.
#[derive(Debug)]
pub struct GnosisExecutorProvider;

impl GnosisExecutorProvider {
    /// Creates a new default optimism executor strategy factory.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(chain_spec: Arc<GnosisChainSpec>) -> BasicBlockExecutorProvider<GnosisEvmConfig> {
        BasicBlockExecutorProvider::new(GnosisEvmConfig::new(chain_spec))
    }
}
