use std::sync::Arc;

use reth_evm::execute::BasicBlockExecutorProvider;

use crate::{evm_config::GnosisEvmConfig, spec::GnosisChainSpec};

/// Helper type with backwards compatible methods to obtain executor providers.
#[derive(Debug)]
pub struct GnosisExecutorProvider;

impl GnosisExecutorProvider {
    /// Creates a new default optimism executor strategy factory.
    pub fn new(chain_spec: Arc<GnosisChainSpec>) -> BasicBlockExecutorProvider<GnosisEvmConfig> {
        BasicBlockExecutorProvider::new(GnosisEvmConfig::new(
            chain_spec,
        ))
    }
}
