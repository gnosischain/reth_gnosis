//! simulateArbitrageAtBlock: 套利模拟 RPC
//!
//! 流程：CREATE arb 合约 -> 执行 startFlashLoan -> 计算利润

use alloy_primitives::{Address, Bytes, U256};
use gnosis_primitives::header::GnosisHeader;
use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::types::ErrorObjectOwned;
use reth_evm::{ConfigureEvm, Evm, EvmFactory};
use reth_provider::{BlockHashReader, HeaderProvider, StateProviderFactory};
use alloy_primitives::keccak256;
use reth_revm::{database::StateProviderDatabase, db::State};
use revm::database::CacheDB;
use revm::Database;
use revm::context::TxEnv;
use revm::context::result::ExecutionResult;
use revm::DatabaseCommit;
use revm_primitives::TxKind;
use revm_primitives::hardfork::SpecId;
use revm::context_interface::block::BlobExcessGasAndPrice;

use tracing;

use crate::blobs::CANCUN_BLOB_PARAMS;
use crate::evm_config::GnosisEvmConfig;

const TX_GAS_LIMIT: u64 = 30_000_000;

/// CREATE 地址: keccak256(rlp([sender, nonce]))[12:]
fn create_address(sender: Address, nonce: u64) -> Address {
    use alloy_primitives::keccak256;
    let mut rlp_addr = vec![0x94];
    rlp_addr.extend_from_slice(sender.as_slice());
    let rlp_nonce: Vec<u8> = if nonce == 0 {
        vec![0x80]
    } else if nonce < 0x80 {
        vec![nonce as u8]
    } else {
        let bytes = nonce.to_be_bytes();
        let nz = bytes.iter().position(|&b| b != 0).unwrap_or(8);
        let mut v = vec![0x80 + (8 - nz) as u8];
        v.extend_from_slice(&bytes[nz..]);
        v
    };
    let list_payload_len = rlp_addr.len() + rlp_nonce.len();
    let mut buf = vec![0xc0 + list_payload_len as u8];
    buf.extend(rlp_addr);
    buf.extend(rlp_nonce);
    let hash = keccak256(&buf);
    Address::from_slice(&hash[12..])
}

/// startFlashLoan(address,uint96,uint96,bool,bytes) selector - V2
const START_FLASH_LOAN_SELECTOR: [u8; 4] = [0x99, 0xf1, 0x80, 0x2a];
/// startFlashLoanV3(address,uint96,uint96,bool,bytes) selector - V3
const START_FLASH_LOAN_V3_SELECTOR: [u8; 4] = [0xbb, 0xa5, 0x9c, 0x67];
/// startFlashLoanV4(address,uint256,bool,bytes) selector — FlashArbV3V4 only
const START_FLASH_LOAN_V4_SELECTOR: [u8; 4] = [0x73, 0xf0, 0x06, 0x21];
/// startFlashLoanBalancer(address,uint256,bool,bytes) — FlashArbitrageUtraLiteUltra Balancer 闪电贷
const START_FLASH_LOAN_BALANCER_SELECTOR: [u8; 4] = [0x93, 0x8f, 0xbc, 0x15];
/// executePath(bool,bytes) selector
const EXECUTE_PATH_SELECTOR: [u8; 4] = [0x91, 0x25, 0x2c, 0x55];
/// WETH() selector: keccak256("WETH()")[0:4]
const WETH_SELECTOR: [u8; 4] = [0xad, 0x5c, 0x46, 0x48];
/// balanceOf(address) selector: keccak256("balanceOf(address)")[0:4]
const BALANCE_OF_SELECTOR: [u8; 4] = [0x70, 0xa0, 0x82, 0x31];
/// transfer(address,uint256) selector: 0xa9059cbb
const TRANSFER_SELECTOR: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb];

/// FlashArbitrageUtraLiteUltra / FlashArbV3V4 常见 error selector -> 名称
fn decode_revert_selector(sel: &[u8]) -> &'static str {
    if sel.len() < 4 {
        return "unknown";
    }
    match sel {
        [0x3f, 0x61, 0x2d, 0x47] => "InsufficientOutput(uint256,uint256,uint256)",
        [0x1c, 0x43, 0xb9, 0x76] => "TransferFailed(address,uint256)",
        [0xcf, 0x47, 0x91, 0x81] => "InsufficientBalance(uint256,uint256)",
        [0xd8, 0x6a, 0xd9, 0xcf] => "UnauthorizedCaller(address)",
        [0xc0, 0xd7, 0xa9, 0x0e] => "InvalidPathData()",
        [0xbc, 0x12, 0x81, 0x47] => "InvalidPoolManager()",
        [0x20, 0xdb, 0x82, 0x67] => "InvalidPath()", // FlashArbV3V4: 路径含 V2 时 revert
        _ => "unknown",
    }
}

/// 静态调用 token.balanceOf(account)，返回余额
fn get_erc20_balance<EV>(
    evm: &mut EV,
    token: Address,
    account: Address,
    caller: Address,
) -> Option<U256>
where
    EV: reth_evm::Evm<DB: revm::Database>,
{
    use alloy_sol_types::SolValue;
    let mut data = Vec::from(BALANCE_OF_SELECTOR);
    data.extend_from_slice(&(account,).abi_encode_params());
    let res = evm.transact_system_call(caller, token, Bytes::from(data));
    let Ok(rs) = res else { return None };
    match &rs.result {
        ExecutionResult::Success { output, .. } => {
            let d = output.data();
            if d.len() >= 32 {
                Some(U256::from_be_slice(d.as_ref()))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// 计算利润：profit = (arb native balance + arb WETH balance) - initial_amount
fn compute_profit_wei<EV>(
    evm: &mut EV,
    arb_address: Address,
    caller: Address,
    request: &ArbitrageSimRequest,
) -> Result<String, ()>
where
    EV: reth_evm::Evm<DB: revm::Database>,
{
    use alloy_sol_types::SolValue;
    use revm::Database;

    let weth_res = evm.transact_system_call(
        caller,
        arb_address,
        Bytes::from(Vec::from(WETH_SELECTOR)),
    );
    let weth_address = match &weth_res {
        Ok(rs) => match &rs.result {
            ExecutionResult::Success { output, .. } => {
                let data = output.data();
                if data.len() >= 32 {
                    let addr_bytes: [u8; 32] = data.as_ref()[..32]
                        .try_into()
                        .map_err(|_| ())?;
                    Address::from_slice(&addr_bytes[12..])
                } else {
                    return Err(());
                }
            }
            _ => return Err(()),
        },
        Err(_) => return Err(()),
    };

    let balance_of_calldata: Vec<u8> = (arb_address,).abi_encode_params();
    let mut balance_of_data = Vec::from(BALANCE_OF_SELECTOR);
    balance_of_data.extend_from_slice(&balance_of_calldata);
    let balance_res = evm.transact_system_call(caller, weth_address, Bytes::from(balance_of_data));
    let weth_balance = match &balance_res {
        Ok(rs) => match &rs.result {
            ExecutionResult::Success { output, .. } => {
                let data = output.data();
                if data.len() >= 32 {
                    U256::from_be_slice(&data.as_ref()[..32])
                } else {
                    U256::ZERO
                }
            }
            _ => U256::ZERO,
        },
        Err(_) => U256::ZERO,
    };

    let native_balance = evm
        .db_mut()
        .basic(arb_address.into())
        .map_err(|_| ())?
        .map(|info| info.balance)
        .unwrap_or(U256::ZERO);

    let initial: U256 = if request.use_flash_loan {
        U256::ZERO
    } else {
        request
            .initial_amount
            .as_ref()
            .and_then(|s| s.parse::<u128>().ok())
            .map(U256::from)
            .unwrap_or(U256::ZERO)
    };

    let total = native_balance.saturating_add(weth_balance);
    let profit = total.saturating_sub(initial);
    Ok(profit.to_string())
}

/// 计算 ERC20 起止路径的利润
fn compute_profit_wei_erc20<EV>(
    evm: &mut EV,
    arb_address: Address,
    caller: Address,
    request: &ArbitrageSimRequest,
) -> Result<String, ()>
where
    EV: reth_evm::Evm<DB: revm::Database>,
{
    let initial_token = request.initial_token.ok_or(())?;
    if initial_token == Address::ZERO {
        return Err(());
    }
    let initial: U256 = request
        .initial_amount
        .as_ref()
        .and_then(|s| s.parse::<u128>().ok())
        .map(U256::from)
        .unwrap_or(U256::ZERO);
    let final_balance = get_erc20_balance(evm, initial_token, arb_address, caller).ok_or(())?;
    let profit = final_balance.saturating_sub(initial);
    Ok(profit.to_string())
}

/// 闪电贷 amount0/amount1（wei 十进制字符串）。不得用 `u64`：`parse::<u64>()` 在金额 > 2^64-1 时会失败并变成 0，
/// 导致 V2 `swap(0,0,...)` 触发 `UniswapV2: INSUFFICIENT_OUTPUT_AMOUNT`。
fn parse_flash_loan_amount_wei(s: Option<&String>) -> U256 {
    s.and_then(|x| x.parse::<U256>().ok()).unwrap_or(U256::ZERO)
}

/// ArbitrageSimRequest
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArbitrageSimRequest {
    pub block_number: u64,
    pub use_flash_loan: bool,
    /// V2 闪电贷：pair 地址
    #[serde(default)]
    pub flash_loan_pair: Option<Address>,
    /// V3 闪电贷：pool 地址（首跳的 V3 pool）
    #[serde(default)]
    pub flash_loan_pool: Option<Address>,
    /// V4 / Balancer：要借的 token 地址（与 `flashLoanAmount` 成对）
    #[serde(default)]
    pub flash_loan_currency: Option<Address>,
    /// 借入数量 (wei)
    #[serde(default)]
    pub flash_loan_amount: Option<String>,
    /// true：`flashLoanCurrency`+`flashLoanAmount` 走 `startFlashLoanBalancer`（Ultra）；false：走 `startFlashLoanV4`（FlashArbV3V4）
    #[serde(default)]
    pub flash_loan_balancer: bool,
    /// goodboy `buildArbRequest` 发送 `flashLoanType: "Balancer"` 时与 `flashLoanBalancer: true` 等价
    #[serde(default)]
    pub flash_loan_type: Option<String>,
    #[serde(default)]
    pub amount0_out: Option<String>,
    #[serde(default)]
    pub amount1_out: Option<String>,
    pub is_first_last_same_eth: bool,
    /// arb 合约 init bytecode，模拟时 CREATE 部署
    pub arb_contract_bytecode: Bytes,
    /// pathData = abi.encode(Hop[])，由调用方编码
    pub path_data: Bytes,
    #[serde(default)]
    pub initial_token: Option<Address>,
    #[serde(default)]
    pub initial_amount: Option<String>,
    #[serde(default)]
    pub funder_address: Option<Address>,
    #[serde(default)]
    pub debug: bool,
}

#[inline]
fn use_balancer_flash_selector(request: &ArbitrageSimRequest) -> bool {
    request.flash_loan_balancer
        || request
            .flash_loan_type
            .as_deref()
            .is_some_and(|s| s.eq_ignore_ascii_case("balancer"))
}

/// 分步 trace 信息
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArbitrageSimTrace {
    pub create_gas_used: u64,
    pub transfer_gas_used: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transfer_reverted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transfer_revert_output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployer_balance_before_transfer: Option<String>,
    pub arb_balance_before_execute_path: Option<String>,
    pub execute_path_gas_used: u64,
}

/// ArbitrageSimResult
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArbitrageSimResult {
    pub success: bool,
    pub profit_wei: String,
    pub gas_used: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revert_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revert_output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<ArbitrageSimTrace>,
}

/// RPC trait for arbitrage simulation
#[rpc(server, namespace = "arb")]
pub trait ArbitrageSimulationApi {
    #[method(name = "simulateArbitrageAtBlock")]
    fn simulate_arbitrage_at_block(
        &self,
        request: ArbitrageSimRequest,
    ) -> RpcResult<ArbitrageSimResult>;
}

pub struct ArbitrageSimulationImpl<Provider> {
    provider: Provider,
    evm_config: GnosisEvmConfig,
}

impl<Provider> ArbitrageSimulationImpl<Provider> {
    pub fn new(provider: Provider, evm_config: GnosisEvmConfig) -> Self {
        Self {
            provider,
            evm_config,
        }
    }
}

impl<Provider> ArbitrageSimulationApiServer for ArbitrageSimulationImpl<Provider>
where
    Provider: BlockHashReader
        + HeaderProvider<Header = GnosisHeader>
        + StateProviderFactory
        + Clone
        + Send
        + Sync
        + 'static,
{
    fn simulate_arbitrage_at_block(
        &self,
        request: ArbitrageSimRequest,
    ) -> RpcResult<ArbitrageSimResult> {
        tracing::info!(
            block = request.block_number,
            use_flash_loan = request.use_flash_loan,
            "arb_simulateArbitrageAtBlock request"
        );
        if request.use_flash_loan {
            let has_v2 = request.flash_loan_pair.is_some();
            let has_v3 = request.flash_loan_pool.is_some();
            let has_v4 = request.flash_loan_currency.is_some() && request.flash_loan_amount.is_some();
            if !has_v2 && !has_v3 && !has_v4 {
                return Err(ErrorObjectOwned::owned(
                    -32602,
                    "useFlashLoan=true requires one of: flashLoanPair (V2), flashLoanPool (V3), or flashLoanCurrency+flashLoanAmount (Ultra Balancer: set flashLoanBalancer=true or flashLoanType=Balancer; FlashArbV3V4: flashLoanBalancer=false)",
                    None::<()>,
                ));
            }
        } else {
            let _ = request
                .initial_token
                .ok_or_else(|| ErrorObjectOwned::owned(-32602, "initialToken required when useFlashLoan=false", None::<()>))?;
            let _ = request
                .initial_amount
                .as_ref()
                .ok_or_else(|| ErrorObjectOwned::owned(-32602, "initialAmount required when useFlashLoan=false", None::<()>))?;
        }

        let block_hash = self
            .provider
            .block_hash(request.block_number)
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

        let mut evm_env = self
            .evm_config
            .evm_env(&header)
            .map_err(|e| ErrorObjectOwned::owned(-32000, format!("EVM env error: {}", e), None::<()>))?;

        // `goodboy/foundry/bsc-arbi-sim/foundry.toml` 使用 evm_version = "cancun"。历史块若在链上
        // Shanghai 激活之前，revm_spec 会偏旧，CREATE 会因 PUSH0/MCOPY 等报 NotActivated；Forge fork
        // 仍按编译目标执行。套利模拟与 Foundry fork / 独立 revm-simulator 对齐：至少使用 Cancun。
        if evm_env.cfg_env.spec < SpecId::CANCUN {
            evm_env
                .cfg_env
                .set_spec_and_mainnet_gas_params(SpecId::CANCUN);
            evm_env
                .cfg_env
                .set_max_blobs_per_tx(CANCUN_BLOB_PARAMS.max_blobs_per_tx);
            if evm_env.block_env.blob_excess_gas_and_price.is_none() {
                let excess_blob_gas = 0u64;
                let blob_gasprice = CANCUN_BLOB_PARAMS.calc_blob_fee(excess_blob_gas);
                evm_env.block_env.blob_excess_gas_and_price =
                    Some(BlobExcessGasAndPrice {
                        excess_blob_gas,
                        blob_gasprice,
                    });
            }
        }

        let beneficiary = evm_env.block_env.beneficiary;
        let state_db = StateProviderDatabase::new(&state_provider);
        let mut cache_db = CacheDB::new(state_db);

        let arb_deployer: Address = Address::from_slice(&[
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2,
        ]);
        let arb_address = create_address(arb_deployer, 0);

        let transfer_caller = request.funder_address.unwrap_or(arb_deployer);
        let use_chain_balance = request.funder_address.is_some();
        if !request.use_flash_loan && !use_chain_balance {
            if let Some(initial_token) = request.initial_token {
                if initial_token != Address::ZERO {
                    use alloy_sol_types::SolValue;
                    let initial_amount: u128 = request
                        .initial_amount
                        .as_ref()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    let slot_hash = keccak256(
                        (arb_deployer, U256::ZERO).abi_encode_params(),
                    );
                    let slot = U256::from_be_slice(slot_hash.as_slice());
                    let _ = cache_db.insert_account_storage(
                        initial_token,
                        slot,
                        U256::from(initial_amount),
                    );
                }
            }
        }

        let db = State::builder().with_database(cache_db).build();

        let basefee = evm_env.block_env.basefee;
        let chain_id = evm_env.cfg_env.chain_id;
        let block_gas_limit = evm_env.block_env.gas_limit;
        let evm_factory = self.evm_config.executor_factory.evm_factory();
        let mut evm = evm_factory.create_evm(db, evm_env);

        let create_tx = TxEnv {
            caller: arb_deployer,
            kind: TxKind::Create,
            data: request.arb_contract_bytecode.clone(),
            value: U256::ZERO,
            gas_limit: TX_GAS_LIMIT.min(block_gas_limit),
            nonce: 0,
            gas_price: basefee.into(),
            gas_priority_fee: Some(0),
            access_list: Default::default(),
            blob_hashes: Vec::new(),
            max_fee_per_blob_gas: 0,
            tx_type: 2,
            chain_id: Some(chain_id),
            authorization_list: Default::default(),
        };

        tracing::info!(block = request.block_number, "step: CREATE arb");
        let create_result = evm.transact(create_tx).map_err(|e| {
            ErrorObjectOwned::owned(-32000, format!("CREATE failed: {}", e), None::<()>)
        })?;
        let create_gas = match &create_result.result {
            ExecutionResult::Success { gas_used, .. } => *gas_used,
            ExecutionResult::Revert { gas_used, .. } | ExecutionResult::Halt { gas_used, .. } => *gas_used,
        };
        tracing::info!(gas = create_gas, "step: CREATE done");

        if let ExecutionResult::Revert { output, gas_used } = &create_result.result {
            return Ok(ArbitrageSimResult {
                success: false,
                profit_wei: "0".to_string(),
                gas_used: *gas_used,
                revert_reason: Some(format!("CREATE reverted: {:?}", output)),
                revert_output: None,
                trace: None,
            });
        }
        if let ExecutionResult::Halt { reason, gas_used } = &create_result.result {
            return Ok(ArbitrageSimResult {
                success: false,
                profit_wei: "0".to_string(),
                gas_used: *gas_used,
                revert_reason: Some(format!("CREATE halt: {:?}", reason)),
                revert_output: None,
                trace: None,
            });
        }

        evm.db_mut().commit(create_result.state);

        use alloy_sol_types::SolValue;

        let mut transfer_gas_used: Option<u64> = None;
        let mut transfer_reverted: Option<bool> = None;
        let mut transfer_revert_output: Option<String> = None;
        let mut deployer_balance_before_transfer: Option<String> = None;
        if !request.use_flash_loan {
            if let Some(initial_token) = request.initial_token {
                if initial_token != Address::ZERO {
                    if request.debug {
                        deployer_balance_before_transfer = get_erc20_balance(
                            &mut evm,
                            initial_token,
                            transfer_caller,
                            arb_deployer,
                        )
                        .map(|u| u.to_string());
                        tracing::info!(
                            deployer_balance = ?deployer_balance_before_transfer,
                            "before transfer"
                        );
                    }
                    let initial_amount: u128 = request
                        .initial_amount
                        .as_ref()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    let mut transfer_data = Vec::from(TRANSFER_SELECTOR);
                    transfer_data.extend_from_slice(
                        &(arb_address, U256::from(initial_amount)).abi_encode_params(),
                    );
                    let transfer_nonce = if use_chain_balance {
                        evm.db_mut()
                            .basic(transfer_caller.into())
                            .ok()
                            .flatten()
                            .map(|a| a.nonce)
                            .unwrap_or(0)
                    } else {
                        1
                    };
                    let transfer_tx = TxEnv {
                        caller: transfer_caller,
                        kind: TxKind::Call(initial_token),
                        data: Bytes::from(transfer_data),
                        value: U256::ZERO,
                        gas_limit: 100_000,
                        nonce: transfer_nonce,
                        gas_price: basefee.into(),
                        gas_priority_fee: Some(0),
                        access_list: Default::default(),
                        blob_hashes: Vec::new(),
                        max_fee_per_blob_gas: 0,
                        tx_type: 2,
                        chain_id: Some(chain_id),
                        authorization_list: Default::default(),
                    };
                    tracing::info!(amount = %initial_amount, "step: transfer to arb");
                    let transfer_result = evm.transact(transfer_tx).map_err(|e| {
                        ErrorObjectOwned::owned(
                            -32000,
                            format!("ERC20 transfer to arb failed: {}", e),
                            None::<()>,
                        )
                    })?;
                    match &transfer_result.result {
                        ExecutionResult::Success { gas_used, .. } => {
                            transfer_gas_used = Some(*gas_used);
                            transfer_reverted = Some(false);
                            tracing::info!(gas = gas_used, "step: transfer done");
                        }
                        ExecutionResult::Revert { output, gas_used } => {
                            transfer_gas_used = Some(*gas_used);
                            transfer_reverted = Some(true);
                            transfer_revert_output =
                                Some(format!("0x{}", hex::encode(output.as_ref())));
                            tracing::warn!(
                                revert = %transfer_revert_output.as_deref().unwrap_or(""),
                                "step: transfer REVERTED"
                            );
                        }
                        ExecutionResult::Halt { reason, gas_used } => {
                            transfer_gas_used = Some(*gas_used);
                            transfer_reverted = Some(true);
                            transfer_revert_output =
                                Some(format!("halt:{:?}", reason));
                            tracing::warn!(reason = ?reason, "step: transfer HALT");
                        }
                    }
                    evm.db_mut().commit(transfer_result.state);
                }
            }
        }

        let arb_balance_before_execute_path = if request.debug {
            request.initial_token.and_then(|t| {
                if t != Address::ZERO {
                    get_erc20_balance(&mut evm, t, arb_address, arb_deployer)
                        .map(|u| u.to_string())
                } else {
                    None
                }
            })
        } else {
            None
        };
        if let Some(ref bal) = arb_balance_before_execute_path {
            tracing::info!(balance = %bal, "arb balance before executePath");
        }

        tracing::info!("step: executePath");
        let call_result = if request.use_flash_loan {
            let (selector, calldata_params): ([u8; 4], Vec<u8>) = if let Some(pair) = request.flash_loan_pair {
                let amount0_out = parse_flash_loan_amount_wei(request.amount0_out.as_ref());
                let amount1_out = parse_flash_loan_amount_wei(request.amount1_out.as_ref());
                let params = (
                    pair,
                    amount0_out,
                    amount1_out,
                    request.is_first_last_same_eth,
                    request.path_data.to_vec(),
                );
                (START_FLASH_LOAN_SELECTOR, params.abi_encode_params())
            } else if let Some(pool) = request.flash_loan_pool {
                let amount0 = parse_flash_loan_amount_wei(request.amount0_out.as_ref());
                let amount1 = parse_flash_loan_amount_wei(request.amount1_out.as_ref());
                let params = (
                    pool,
                    amount0,
                    amount1,
                    request.is_first_last_same_eth,
                    request.path_data.to_vec(),
                );
                (START_FLASH_LOAN_V3_SELECTOR, params.abi_encode_params())
            } else if let (Some(currency), Some(amount_str)) = (&request.flash_loan_currency, &request.flash_loan_amount) {
                let amount: U256 = amount_str.parse().unwrap_or(U256::ZERO);
                let params = (
                    *currency,
                    amount,
                    request.is_first_last_same_eth,
                    request.path_data.to_vec(),
                );
                if use_balancer_flash_selector(&request) {
                    (START_FLASH_LOAN_BALANCER_SELECTOR, params.abi_encode_params())
                } else {
                    (START_FLASH_LOAN_V4_SELECTOR, params.abi_encode_params())
                }
            } else {
                return Err(ErrorObjectOwned::owned(
                    -32602,
                    "useFlashLoan=true requires one of: flashLoanPair (V2), flashLoanPool (V3), or flashLoanCurrency+flashLoanAmount (Ultra Balancer: set flashLoanBalancer=true or flashLoanType=Balancer; FlashArbV3V4: flashLoanBalancer=false)",
                    None::<()>,
                ));
            };
            let mut calldata = Vec::from(selector);
            calldata.extend_from_slice(&calldata_params);
            let call_tx = TxEnv {
                caller: arb_deployer,
                kind: TxKind::Call(arb_address),
                data: Bytes::from(calldata),
                value: U256::ZERO,
                gas_limit: TX_GAS_LIMIT.min(block_gas_limit),
                nonce: 1,
                gas_price: basefee.into(),
                gas_priority_fee: Some(0),
                access_list: Default::default(),
                blob_hashes: Vec::new(),
                max_fee_per_blob_gas: 0,
                tx_type: 2,
                chain_id: Some(chain_id),
                authorization_list: Default::default(),
            };
            evm.transact(call_tx).map_err(|e| {
                ErrorObjectOwned::owned(-32000, format!("startFlashLoan failed: {}", e), None::<()>)
            })?
        } else {
            let initial_token = request.initial_token.unwrap();
            let initial_amount: u128 = request
                .initial_amount
                .as_ref()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            if initial_token == Address::ZERO {
                let fund_tx = TxEnv {
                    caller: beneficiary,
                    kind: TxKind::Call(arb_deployer),
                    data: Bytes::new(),
                    value: U256::from(initial_amount),
                    gas_limit: 21000,
                    nonce: 0,
                    gas_price: basefee.into(),
                    gas_priority_fee: Some(0),
                    access_list: Default::default(),
                    blob_hashes: Vec::new(),
                    max_fee_per_blob_gas: 0,
                    tx_type: 2,
                    chain_id: Some(chain_id),
                    authorization_list: Default::default(),
                };
                let fund_result = evm.transact(fund_tx).map_err(|e| {
                    ErrorObjectOwned::owned(
                        -32000,
                        format!("Fund arb_deployer (native) failed: {}", e),
                        None::<()>,
                    )
                })?;
                evm.db_mut().commit(fund_result.state);
            }

            let mut calldata = Vec::from(EXECUTE_PATH_SELECTOR);
            calldata.extend_from_slice(
                &(request.is_first_last_same_eth, request.path_data.to_vec()).abi_encode_params(),
            );
            let value = if initial_token == Address::ZERO {
                U256::from(initial_amount)
            } else {
                U256::ZERO
            };
            let execute_path_nonce = if use_chain_balance { 1 } else { 2 };
            let call_tx = TxEnv {
                caller: arb_deployer,
                kind: TxKind::Call(arb_address),
                data: Bytes::from(calldata),
                value,
                gas_limit: TX_GAS_LIMIT.min(block_gas_limit),
                nonce: execute_path_nonce,
                gas_price: basefee.into(),
                gas_priority_fee: Some(0),
                access_list: Default::default(),
                blob_hashes: Vec::new(),
                max_fee_per_blob_gas: 0,
                tx_type: 2,
                chain_id: Some(chain_id),
                authorization_list: Default::default(),
            };
            evm.transact(call_tx).map_err(|e| {
                ErrorObjectOwned::owned(-32000, format!("executePath failed: {}", e), None::<()>)
            })?
        };

        let (success, gas_used, revert_reason, revert_output) = match &call_result.result {
            ExecutionResult::Success { gas_used, .. } => (true, *gas_used, None, None),
            ExecutionResult::Revert { output, gas_used } => {
                let hex_out = format!("0x{}", hex::encode(output.as_ref()));
                let reason = if output.as_ref().is_empty() {
                    "reverted: 0x (empty)".to_string()
                } else if output.as_ref().len() >= 4 {
                    let sel = &output.as_ref()[..4];
                    let reason = decode_revert_selector(sel);
                    format!("reverted: {} {}", reason, hex_out)
                } else {
                    format!("reverted: {}", hex_out)
                };
                (
                    false,
                    *gas_used,
                    Some(reason),
                    Some(hex_out),
                )
            }
            ExecutionResult::Halt { reason, gas_used } => (
                false,
                *gas_used,
                Some(format!("halt: {:?}", reason)),
                None,
            ),
        };

        evm.db_mut().commit(call_result.state);

        let profit_wei = if success {
            if request.is_first_last_same_eth {
                compute_profit_wei(&mut evm, arb_address, arb_deployer, &request)
                    .unwrap_or_else(|_| "0".to_string())
            } else if request.initial_token.is_some() && request.initial_token != Some(Address::ZERO) {
                compute_profit_wei_erc20(&mut evm, arb_address, arb_deployer, &request)
                    .unwrap_or_else(|_| "0".to_string())
            } else {
                "0".to_string()
            }
        } else {
            "0".to_string()
        };

        let trace = if request.debug {
            Some(ArbitrageSimTrace {
                create_gas_used: create_gas,
                transfer_gas_used,
                transfer_reverted,
                transfer_revert_output,
                deployer_balance_before_transfer,
                arb_balance_before_execute_path,
                execute_path_gas_used: gas_used,
            })
        } else {
            None
        };

        Ok(ArbitrageSimResult {
            success,
            profit_wei,
            gas_used,
            revert_reason,
            revert_output,
            trace,
        })
    }
}
