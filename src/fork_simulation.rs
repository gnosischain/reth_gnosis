//! Fork simulation RPC: eth_callAtBlock, eth_callScriptAtBlock, eth_forkSyncStatus
//!
//! Integrated into reth_gnosis for same-block-height as eth_blockNumber.

use alloy_eips::eip4788;
use alloy_primitives::{Address, Bytes, U256};
use gnosis_primitives::header::GnosisHeader;
use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::types::ErrorObjectOwned;
use reth_evm::{ConfigureEvm, Evm, EvmFactory};
use reth_provider::{BlockHashReader, BlockNumReader, HeaderProvider, StateProviderFactory};
use reth_revm::{database::StateProviderDatabase, db::State};
use revm::context::TxEnv;
use revm::context::result::ExecutionResult;
use revm_primitives::TxKind;

use crate::evm_config::GnosisEvmConfig;

const TX_GAS_LIMIT: u64 = 30_000_000;

/// Call request for eth_callAtBlock
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallRequest {
    #[serde(default)]
    pub from: Option<Address>,
    pub to: Address,
    #[serde(default)]
    pub data: Option<Bytes>,
    #[serde(default)]
    pub value: Option<U256>,
    #[serde(default)]
    pub gas: Option<u64>,
}

/// Fork sync status result
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkSyncStatus {
    pub max_block_number: u64,
    pub best_block_number: u64,
    pub last_block_number: u64,
}

/// Result of a fork simulation call
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallResult {
    pub output: Bytes,
    pub gas_used: u64,
    pub status: String,
}

/// RPC trait for fork simulation methods
#[rpc(server, namespace = "eth")]
pub trait ForkSimulationApi {
    #[method(name = "forkSyncStatus")]
    fn fork_sync_status(&self) -> RpcResult<ForkSyncStatus>;

    #[method(name = "callAtBlock")]
    fn call_at_block(
        &self,
        call: CallRequest,
        block_number: u64,
    ) -> RpcResult<CallResult>;

    #[method(name = "callScriptAtBlock")]
    fn call_script_at_block(
        &self,
        bytecode: Bytes,
        block_number: u64,
    ) -> RpcResult<CallResult>;
}

/// Implementation of fork simulation RPC using node's provider (same DB as eth_blockNumber).
pub struct ForkSimulationImpl<Provider> {
    provider: Provider,
    evm_config: GnosisEvmConfig,
}

impl<Provider> ForkSimulationImpl<Provider> {
    pub fn new(provider: Provider, evm_config: GnosisEvmConfig) -> Self {
        Self {
            provider,
            evm_config,
        }
    }
}

impl<Provider> ForkSimulationApiServer for ForkSimulationImpl<Provider>
where
    Provider: BlockNumReader
        + BlockHashReader
        + HeaderProvider<Header = GnosisHeader>
        + StateProviderFactory
        + Clone
        + Send
        + Sync
        + 'static,
{
    fn fork_sync_status(&self) -> RpcResult<ForkSyncStatus> {
        let best_block_number = self
            .provider
            .best_block_number()
            .map_err(|e| ErrorObjectOwned::owned(-32000, format!("Provider error: {}", e), None::<()>))?;
        let last_block_number = self
            .provider
            .last_block_number()
            .map_err(|e| ErrorObjectOwned::owned(-32000, format!("Provider error: {}", e), None::<()>))?;
        Ok(ForkSyncStatus {
            max_block_number: best_block_number,
            best_block_number,
            last_block_number,
        })
    }

    fn call_at_block(&self, call: CallRequest, block_number: u64) -> RpcResult<CallResult> {
        let block_hash = self
            .provider
            .block_hash(block_number)
            .map_err(|e| ErrorObjectOwned::owned(-32000, format!("Provider error: {}", e), None::<()>))?
            .ok_or_else(|| ErrorObjectOwned::owned(-32000, "Block not found", None::<()>))?;

        let header = self
            .provider
            .header(block_hash)
            .map_err(|e| ErrorObjectOwned::owned(-32000, format!("Provider error: {}", e), None::<()>))?
            .ok_or_else(|| ErrorObjectOwned::owned(-32000, "Block not found", None::<()>))?;

        let state_provider = self
            .provider
            .history_by_block_hash(block_hash)
            .map_err(|e| {
                ErrorObjectOwned::owned(-32000, format!("State not available: {}", e), None::<()>)
            })?;

        let state_db = StateProviderDatabase::new(&state_provider);
        let db = State::builder().with_database(state_db).build();

        let evm_env = self
            .evm_config
            .evm_env(&header)
            .map_err(|e| ErrorObjectOwned::owned(-32000, format!("EVM env error: {}", e), None::<()>))?;

        let basefee = evm_env.block_env.basefee;
        let chain_id = evm_env.cfg_env.chain_id;
        let block_gas_limit = evm_env.block_env.gas_limit;

        let evm_factory = self.evm_config.executor_factory.evm_factory();
        let mut evm = evm_factory.create_evm(db, evm_env);

        let caller = call.from.unwrap_or(Address::ZERO);
        let data = call.data.unwrap_or_default();
        let value = call.value.unwrap_or(U256::ZERO);
        let gas_limit = call
            .gas
            .unwrap_or(TX_GAS_LIMIT)
            .min(block_gas_limit);

        let tx = TxEnv {
            caller,
            kind: TxKind::Call(call.to),
            data: data.clone(),
            value,
            gas_limit,
            nonce: 0,
            gas_price: basefee.into(),
            gas_priority_fee: Some(0),
            access_list: Default::default(),
            blob_hashes: Vec::new(),
            max_fee_per_blob_gas: 0,
            tx_type: 2, // EIP-1559
            chain_id: Some(chain_id),
            authorization_list: Default::default(),
        };

        let result = evm.transact(tx).map_err(|e| {
            ErrorObjectOwned::owned(-32000, format!("Execution failed: {}", e), None::<()>)
        })?;

        Ok(match result.result {
            ExecutionResult::Success {
                output, gas_used, ..
            } => CallResult {
                output: output.into_data().into(),
                gas_used,
                status: "success".to_string(),
            },
            ExecutionResult::Halt {
                reason, gas_used, ..
            } => CallResult {
                output: Bytes::new(),
                gas_used,
                status: format!("halt: {:?}", reason),
            },
            ExecutionResult::Revert { output, gas_used } => CallResult {
                output: output.into(),
                gas_used,
                status: "reverted".to_string(),
            },
        })
    }

    fn call_script_at_block(&self, bytecode: Bytes, block_number: u64) -> RpcResult<CallResult> {
        let block_hash = self
            .provider
            .block_hash(block_number)
            .map_err(|e| ErrorObjectOwned::owned(-32000, format!("Provider error: {}", e), None::<()>))?
            .ok_or_else(|| ErrorObjectOwned::owned(-32000, "Block not found", None::<()>))?;

        let header = self
            .provider
            .header(block_hash)
            .map_err(|e| ErrorObjectOwned::owned(-32000, format!("Provider error: {}", e), None::<()>))?
            .ok_or_else(|| ErrorObjectOwned::owned(-32000, "Block not found", None::<()>))?;

        let state_provider = self
            .provider
            .history_by_block_hash(block_hash)
            .map_err(|e| {
                ErrorObjectOwned::owned(-32000, format!("State not available: {}", e), None::<()>)
            })?;

        let state_db = StateProviderDatabase::new(&state_provider);
        let db = State::builder().with_database(state_db).build();

        let evm_env = self
            .evm_config
            .evm_env(&header)
            .map_err(|e| ErrorObjectOwned::owned(-32000, format!("EVM env error: {}", e), None::<()>))?;

        let basefee = evm_env.block_env.basefee;
        let chain_id = evm_env.cfg_env.chain_id;
        let block_gas_limit = evm_env.block_env.gas_limit;

        let evm_factory = self.evm_config.executor_factory.evm_factory();
        let mut evm = evm_factory.create_evm(db, evm_env);

        let tx = TxEnv {
            caller: eip4788::SYSTEM_ADDRESS,
            kind: TxKind::Create,
            data: bytecode,
            value: U256::ZERO,
            gas_limit: TX_GAS_LIMIT.min(block_gas_limit),
            nonce: 0,
            gas_price: basefee.into(),
            gas_priority_fee: Some(0),
            access_list: Default::default(),
            blob_hashes: Vec::new(),
            max_fee_per_blob_gas: 0,
            tx_type: 2, // EIP-1559
            chain_id: Some(chain_id),
            authorization_list: Default::default(),
        };

        let result = evm.transact(tx).map_err(|e| {
            ErrorObjectOwned::owned(-32000, format!("Execution failed: {}", e), None::<()>)
        })?;

        Ok(match result.result {
            ExecutionResult::Success {
                output, gas_used, ..
            } => CallResult {
                output: output.into_data().into(),
                gas_used,
                status: "success".to_string(),
            },
            ExecutionResult::Halt {
                reason, gas_used, ..
            } => CallResult {
                output: Bytes::new(),
                gas_used,
                status: format!("halt: {:?}", reason),
            },
            ExecutionResult::Revert { output, gas_used } => CallResult {
                output: output.into(),
                gas_used,
                status: "reverted".to_string(),
            },
        })
    }
}
