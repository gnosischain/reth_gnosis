use alloy_eips::BlockId;
use alloy_eips::BlockNumberOrTag;
use alloy_primitives::{Address, Bytes, B256, U256, U64};
use alloy_rpc_types_eth::{
    state::StateOverride, BlockOverrides, Bundle, EIP1186AccountProofResponse, EthCallResponse,
    FeeHistory, Filter, FilterId, Index, Log, StateContext, TransactionRequest,
};
use alloy_rpc_types_trace::filter::TraceFilter;
use alloy_rpc_types_trace::geth::{
    GethDebugTracingCallOptions, GethDebugTracingOptions, GethTrace, TraceResult,
};
use alloy_rpc_types_trace::parity::{
    LocalizedTransactionTrace, TraceResults, TraceResultsWithTransactionHash, TraceType,
};

use alloy_primitives::map::HashSet as AHashSet;
use alloy_serde::JsonStorageKey;
use eyre::Result;
use jsonrpsee::{core::RpcResult, RpcModule};
use reth_rpc_api::{DebugApiServer, TraceApiServer};
use reth_rpc_eth_api::{
    types::{EthApiTypes, RpcTransaction},
    EthApiServer, EthFilterApiServer, FullEthApiServer,
};
use std::future::Future;

pub const GENESIS_BLOCK: u64 = 6306357;

fn is_block_disallowed(b: &BlockNumberOrTag) -> bool {
    match b {
        BlockNumberOrTag::Number(n) => *n < GENESIS_BLOCK,
        BlockNumberOrTag::Earliest => true,
        _ => false,
    }
}

fn value_to_block_number_or_tag(v: &serde_json::Value) -> Option<BlockNumberOrTag> {
    match v {
        serde_json::Value::String(s) => {
            let s_lc = s.to_ascii_lowercase();
            match s_lc.as_str() {
                "earliest" => Some(BlockNumberOrTag::Earliest),
                "latest" => Some(BlockNumberOrTag::Latest),
                "pending" => Some(BlockNumberOrTag::Pending),
                "finalized" => Some(BlockNumberOrTag::Finalized),
                "safe" => Some(BlockNumberOrTag::Safe),
                _ => {
                    if let Some(stripped) = s.strip_prefix("0x") {
                        u64::from_str_radix(stripped, 16)
                            .ok()
                            .map(BlockNumberOrTag::Number)
                    } else {
                        s.parse::<u64>().ok().map(BlockNumberOrTag::Number)
                    }
                }
            }
        }
        serde_json::Value::Number(n) => n.as_u64().map(BlockNumberOrTag::Number),
        _ => None,
    }
}

fn filter_has_disallowed_block_range(filter: &serde_json::Value) -> bool {
    if let Some(obj) = filter.as_object() {
        if let Some(from_block) = obj.get("fromBlock").and_then(value_to_block_number_or_tag) {
            if is_block_disallowed(&from_block) {
                return true;
            }
        }
        if let Some(to_block) = obj.get("toBlock").and_then(value_to_block_number_or_tag) {
            if is_block_disallowed(&to_block) {
                return true;
            }
        }
    }
    false
}

pub fn register_eth_get_block_by_number<F, Fut, R>(
    m: &mut RpcModule<()>,
    block_by_number: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(BlockNumberOrTag, bool) -> Fut,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("eth_getBlockByNumber", move |params, _conn, _ctx| {
        let call = block_by_number.clone();
        async move {
            let (number, full): (BlockNumberOrTag, bool) = params.parse()?;
            if is_block_disallowed(&number) {
                return Ok(None);
            }
            call(number, full).await
        }
    })?;
    Ok(())
}

pub fn register_eth_get_transaction_by_block_number_and_index<F, Fut, R>(
    m: &mut RpcModule<()>,
    tx_by_block_number_and_index: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(BlockNumberOrTag, Index) -> Fut,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method(
        "eth_getTransactionByBlockNumberAndIndex",
        move |params, _conn, _ctx| {
            let call = tx_by_block_number_and_index.clone();
            async move {
                let (number, index): (BlockNumberOrTag, Index) = params.parse()?;
                if is_block_disallowed(&number) {
                    return Ok(None);
                }
                call(number, index).await
            }
        },
    )?;
    Ok(())
}

pub fn register_eth_get_block_transaction_count_by_number<F, Fut>(
    m: &mut RpcModule<()>,
    tx_count_by_block_number: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(BlockNumberOrTag) -> Fut,
    Fut: Future<Output = RpcResult<Option<U256>>> + Send + 'static,
{
    m.register_async_method(
        "eth_getBlockTransactionCountByNumber",
        move |params, _conn, _ctx| {
            let call = tx_count_by_block_number.clone();
            async move {
                let (number,): (BlockNumberOrTag,) = params.parse()?;
                if is_block_disallowed(&number) {
                    return Ok(None);
                }
                call(number).await
            }
        },
    )?;
    Ok(())
}

pub fn register_eth_get_uncle_count_by_block_number_via_block<F, Fut, B>(
    m: &mut RpcModule<()>,
    block_by_number: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(BlockNumberOrTag, bool) -> Fut,
    Fut: Future<Output = RpcResult<Option<B>>> + Send + 'static,
    B: serde::Serialize + Send + 'static,
{
    m.register_async_method(
        "eth_getUncleCountByBlockNumber",
        move |params, _conn, _ctx| {
            let call = block_by_number.clone();
            async move {
                let (number,): (BlockNumberOrTag,) = params.parse()?;
                if is_block_disallowed(&number) {
                    return Ok(None);
                }
                match call(number, false).await {
                    Ok(Some(block)) => {
                        let v = serde_json::to_value(block).unwrap_or(serde_json::json!({}));
                        let count = v
                            .get("uncles")
                            .and_then(|a| a.as_array())
                            .map(|a| a.len())
                            .or_else(|| v.get("ommers").and_then(|a| a.as_array()).map(|a| a.len()))
                            .unwrap_or(0);
                        Ok(Some(format!("0x{:x}", count)))
                    }
                    Ok(None) => Ok(None),
                    Err(err) => Err(err),
                }
            }
        },
    )?;
    Ok(())
}

pub fn register_eth_get_block_receipts<F, Fut, R>(
    m: &mut RpcModule<()>,
    get_block_receipts: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(BlockId) -> Fut,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("eth_getBlockReceipts", move |params, _conn, _ctx| {
        let call = get_block_receipts.clone();
        async move {
            let (block_id,): (BlockId,) = params.parse()?;
            if let BlockId::Number(num_tag) = &block_id {
                if is_block_disallowed(num_tag) {
                    return Ok(None);
                }
            }
            call(block_id).await
        }
    })?;
    Ok(())
}

pub fn register_eth_get_balance<F, Fut>(m: &mut RpcModule<()>, get_balance: F) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(Address, Option<BlockId>) -> Fut,
    Fut: Future<Output = RpcResult<U256>> + Send + 'static,
{
    m.register_async_method("eth_getBalance", move |params, _conn, _ctx| {
        let call = get_balance.clone();
        async move {
            let (address, block): (Address, Option<BlockId>) = params.parse()?;
            if let Some(BlockId::Number(num)) = &block {
                if is_block_disallowed(num) {
                    return Ok(U256::ZERO);
                }
            }
            call(address, block).await
        }
    })?;
    Ok(())
}

pub fn register_eth_get_code<F, Fut>(m: &mut RpcModule<()>, get_code: F) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(Address, Option<BlockId>) -> Fut,
    Fut: Future<Output = RpcResult<Bytes>> + Send + 'static,
{
    m.register_async_method("eth_getCode", move |params, _conn, _ctx| {
        let call = get_code.clone();
        async move {
            let (address, block): (Address, Option<BlockId>) = params.parse()?;
            if let Some(BlockId::Number(num)) = &block {
                if is_block_disallowed(num) {
                    return Ok(Bytes::default());
                }
            }
            call(address, block).await
        }
    })?;
    Ok(())
}

pub fn register_eth_get_storage_at<F, Fut>(m: &mut RpcModule<()>, get_storage_at: F) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(Address, JsonStorageKey, Option<BlockId>) -> Fut,
    Fut: Future<Output = RpcResult<B256>> + Send + 'static,
{
    m.register_async_method("eth_getStorageAt", move |params, _conn, _ctx| {
        let call = get_storage_at.clone();
        async move {
            let (address, slot, block): (Address, JsonStorageKey, Option<BlockId>) =
                params.parse()?;
            if let Some(BlockId::Number(num)) = &block {
                if is_block_disallowed(num) {
                    return Ok(B256::ZERO);
                }
            }
            call(address, slot, block).await
        }
    })?;
    Ok(())
}

pub fn register_eth_get_transaction_count<F, Fut>(
    m: &mut RpcModule<()>,
    get_tx_count: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(Address, Option<BlockId>) -> Fut,
    Fut: Future<Output = RpcResult<U64>> + Send + 'static,
{
    m.register_async_method("eth_getTransactionCount", move |params, _conn, _ctx| {
        let call = get_tx_count.clone();
        async move {
            let (address, block): (Address, Option<BlockId>) = params.parse()?;
            if let Some(BlockId::Number(num)) = &block {
                if is_block_disallowed(num) {
                    return Ok(U64::ZERO);
                }
            }
            call(address, block).await
        }
    })?;
    Ok(())
}

pub fn register_eth_get_proof<F, Fut>(m: &mut RpcModule<()>, get_proof: F) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(Address, Vec<B256>, Option<BlockId>) -> Fut,
    Fut: Future<Output = RpcResult<EIP1186AccountProofResponse>> + Send + 'static,
{
    m.register_async_method("eth_getProof", move |params, _conn, _ctx| {
        let call = get_proof.clone();
        async move {
            let (address, slots, block): (Address, Vec<B256>, Option<BlockId>) = params.parse()?;
            if let Some(BlockId::Number(num)) = &block {
                if is_block_disallowed(num) {
                    return Ok(EIP1186AccountProofResponse::default());
                }
            }
            call(address, slots, block).await
        }
    })?;
    Ok(())
}

pub fn register_eth_new_filter<F, Fut>(m: &mut RpcModule<()>, new_filter: F) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(Filter) -> Fut,
    Fut: Future<Output = RpcResult<FilterId>> + Send + 'static,
{
    m.register_async_method("eth_newFilter", move |params, _conn, _ctx| {
        let call = new_filter.clone();
        async move {
            let (fv,): (serde_json::Value,) = params.parse()?;
            if filter_has_disallowed_block_range(&fv) {
                return Ok(FilterId::Num(0));
            }
            let filter: Filter = match serde_json::from_value(fv) {
                Ok(v) => v,
                Err(_) => return Ok(FilterId::Num(0)),
            };
            call(filter).await
        }
    })?;
    Ok(())
}

pub fn register_eth_call<F, Fut>(m: &mut RpcModule<()>, call_fn: F) -> Result<()>
where
    F: Clone
        + Send
        + Sync
        + 'static
        + Fn(
            TransactionRequest,
            Option<BlockId>,
            Option<StateOverride>,
            Option<Box<BlockOverrides>>,
        ) -> Fut,
    Fut: Future<Output = RpcResult<Bytes>> + Send + 'static,
{
    m.register_async_method("eth_call", move |params, _conn, _ctx| {
        let call = call_fn.clone();
        async move {
            let (request, block, state_overrides, block_overrides): (
                TransactionRequest,
                Option<BlockId>,
                Option<StateOverride>,
                Option<Box<BlockOverrides>>,
            ) = params.parse()?;
            if let Some(BlockId::Number(num)) = &block {
                if is_block_disallowed(num) {
                    return Ok(Bytes::default());
                }
            }
            call(request, block, state_overrides, block_overrides).await
        }
    })?;
    Ok(())
}

pub fn register_eth_call_many<F, Fut>(m: &mut RpcModule<()>, call_many_fn: F) -> Result<()>
where
    F: Clone
        + Send
        + Sync
        + 'static
        + Fn(Vec<Bundle>, Option<StateContext>, Option<StateOverride>) -> Fut,
    Fut: Future<Output = RpcResult<Vec<Vec<EthCallResponse>>>> + Send + 'static,
{
    m.register_async_method("eth_callMany", move |params, _conn, _ctx| {
        let call = call_many_fn.clone();
        async move {
            let (calls, state_context, state_override): (
                Vec<Bundle>,
                Option<StateContext>,
                Option<StateOverride>,
            ) = params.parse()?;

            if let Some(ref context) = state_context {
                if let Some(alloy_eips::BlockId::Number(num)) = &context.block_number {
                    if is_block_disallowed(num) {
                        return Ok(vec![]);
                    }
                }
            }

            call(calls, state_context, state_override).await
        }
    })?;
    Ok(())
}

pub fn register_eth_estimate_gas<F, Fut>(m: &mut RpcModule<()>, estimate_fn: F) -> Result<()>
where
    F: Clone
        + Send
        + Sync
        + 'static
        + Fn(TransactionRequest, Option<BlockId>, Option<StateOverride>) -> Fut,
    Fut: Future<Output = RpcResult<U256>> + Send + 'static,
{
    m.register_async_method("eth_estimateGas", move |params, _conn, _ctx| {
        let call = estimate_fn.clone();
        async move {
            let (request, block, state_override): (
                TransactionRequest,
                Option<BlockId>,
                Option<StateOverride>,
            ) = params.parse()?;
            if let Some(BlockId::Number(num)) = &block {
                if is_block_disallowed(num) {
                    return Ok(U256::ZERO);
                }
            }
            call(request, block, state_override).await
        }
    })?;
    Ok(())
}

pub fn register_eth_fee_history<F, Fut>(m: &mut RpcModule<()>, fee_history_fn: F) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(U64, BlockNumberOrTag, Option<Vec<f64>>) -> Fut,
    Fut: Future<Output = RpcResult<FeeHistory>> + Send + 'static,
{
    m.register_async_method("eth_feeHistory", move |params, _conn, _ctx| {
        let call = fee_history_fn.clone();
        async move {
            let (block_count, newest_block, reward_percentiles): (
                U64,
                BlockNumberOrTag,
                Option<Vec<f64>>,
            ) = params.parse()?;
            if is_block_disallowed(&newest_block) {
                return Ok(FeeHistory::default());
            }
            call(block_count, newest_block, reward_percentiles).await
        }
    })?;
    Ok(())
}

pub fn register_eth_get_logs<F, Fut>(m: &mut RpcModule<()>, get_logs_fn: F) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(Filter) -> Fut,
    Fut: Future<Output = RpcResult<Vec<Log>>> + Send + 'static,
{
    m.register_async_method("eth_getLogs", move |params, _conn, _ctx| {
        let call = get_logs_fn.clone();
        async move {
            let (fv,): (serde_json::Value,) = params.parse()?;
            if filter_has_disallowed_block_range(&fv) {
                return Ok(vec![]);
            }
            let filter: Filter = match serde_json::from_value(fv) {
                Ok(v) => v,
                Err(_) => return Ok(vec![]),
            };
            call(filter).await
        }
    })?;
    Ok(())
}

// --- Trace helpers (generic over types) ---

pub fn register_trace_replay_block_transactions<F, Fut>(
    m: &mut RpcModule<()>,
    replay_fn: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(BlockId, AHashSet<TraceType>) -> Fut,
    Fut: Future<Output = RpcResult<Option<Vec<TraceResultsWithTransactionHash>>>> + Send + 'static,
{
    m.register_async_method(
        "trace_replayBlockTransactions",
        move |params, _conn, _ctx| {
            let call = replay_fn.clone();
            async move {
                let (block_id, trace_types): (BlockId, AHashSet<TraceType>) = params.parse()?;
                if let BlockId::Number(num) = &block_id {
                    if is_block_disallowed(num) {
                        return Ok(None);
                    }
                }
                call(block_id, trace_types).await
            }
        },
    )?;
    Ok(())
}

pub fn register_debug_trace_block_by_number<F, Fut>(
    m: &mut RpcModule<()>,
    trace_fn: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(BlockNumberOrTag, Option<GethDebugTracingOptions>) -> Fut,
    Fut: Future<Output = RpcResult<Vec<TraceResult>>> + Send + 'static,
{
    m.register_async_method("debug_traceBlockByNumber", move |params, _conn, _ctx| {
        let call = trace_fn.clone();
        async move {
            let (block_number, opts): (BlockNumberOrTag, Option<GethDebugTracingOptions>) =
                params.parse()?;
            if is_block_disallowed(&block_number) {
                return Ok(vec![]);
            }
            call(block_number, opts).await
        }
    })?;
    Ok(())
}

pub fn register_debug_trace_call<F, Fut>(m: &mut RpcModule<()>, trace_call_fn: F) -> Result<()>
where
    F: Clone
        + Send
        + Sync
        + 'static
        + Fn(TransactionRequest, Option<BlockId>, Option<GethDebugTracingCallOptions>) -> Fut,
    Fut: Future<Output = RpcResult<GethTrace>> + Send + 'static,
{
    m.register_async_method("debug_traceCall", move |params, _conn, _ctx| {
        let call = trace_call_fn.clone();
        async move {
            let (request, block_number, opts): (
                TransactionRequest,
                Option<BlockId>,
                Option<GethDebugTracingCallOptions>,
            ) = params.parse()?;
            if let Some(BlockId::Number(num)) = &block_number {
                if is_block_disallowed(num) {
                    return Ok(GethTrace::default());
                }
            }
            call(request, block_number, opts).await
        }
    })?;
    Ok(())
}

pub fn register_debug_trace_call_many<F, Fut>(
    m: &mut RpcModule<()>,
    trace_call_many_fn: F,
) -> Result<()>
where
    F: Clone
        + Send
        + Sync
        + 'static
        + Fn(Vec<Bundle>, Option<StateContext>, Option<GethDebugTracingCallOptions>) -> Fut,
    Fut: Future<Output = RpcResult<Vec<Vec<GethTrace>>>> + Send + 'static,
{
    m.register_async_method("debug_traceCallMany", move |params, _conn, _ctx| {
        let call = trace_call_many_fn.clone();
        async move {
            let (calls, state_context, opts): (
                Vec<Bundle>,
                Option<StateContext>,
                Option<GethDebugTracingCallOptions>,
            ) = params.parse()?;
            if let Some(ref context) = state_context {
                if let Some(alloy_eips::BlockId::Number(num)) = &context.block_number {
                    if is_block_disallowed(num) {
                        return Ok(vec![]);
                    }
                }
            }
            call(calls, state_context, opts).await
        }
    })?;
    Ok(())
}

pub fn register_trace_block<F, Fut>(m: &mut RpcModule<()>, block_fn: F) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(BlockId) -> Fut,
    Fut: Future<Output = RpcResult<Option<Vec<LocalizedTransactionTrace>>>> + Send + 'static,
{
    m.register_async_method("trace_block", move |params, _conn, _ctx| {
        let call = block_fn.clone();
        async move {
            let (block_id,): (BlockId,) = params.parse()?;
            if let BlockId::Number(num) = &block_id {
                if is_block_disallowed(num) {
                    return Ok(None);
                }
            }
            call(block_id).await
        }
    })?;
    Ok(())
}

pub fn register_trace_filter<F, Fut>(m: &mut RpcModule<()>, filter_fn: F) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(TraceFilter) -> Fut,
    Fut: Future<Output = RpcResult<Vec<LocalizedTransactionTrace>>> + Send + 'static,
{
    m.register_async_method("trace_filter", move |params, _conn, _ctx| {
        let call = filter_fn.clone();
        async move {
            let (fv,): (serde_json::Value,) = params.parse()?;
            if filter_has_disallowed_block_range(&fv) {
                return Ok(vec![]);
            }
            let trace_filter: TraceFilter = match serde_json::from_value(fv) {
                Ok(v) => v,
                Err(_) => return Ok(vec![]),
            };
            call(trace_filter).await
        }
    })?;
    Ok(())
}

pub fn register_trace_call_many<F, Fut>(m: &mut RpcModule<()>, call_many_fn: F) -> Result<()>
where
    F: Clone
        + Send
        + Sync
        + 'static
        + Fn(Vec<(TransactionRequest, AHashSet<TraceType>)>, Option<BlockId>) -> Fut,
    Fut: Future<Output = RpcResult<Vec<TraceResults>>> + Send + 'static,
{
    m.register_async_method("trace_callMany", move |params, _conn, _ctx| {
        let call = call_many_fn.clone();
        async move {
            let (calls, block_id): (
                Vec<(TransactionRequest, AHashSet<TraceType>)>,
                Option<BlockId>,
            ) = params.parse()?;
            if let Some(BlockId::Number(num)) = &block_id {
                if is_block_disallowed(num) {
                    return Ok(vec![]);
                }
            }
            call(calls, block_id).await
        }
    })?;
    Ok(())
}

pub fn build_guarded_module<E, T, D, F>(
    eth: E,
    trace: T,
    debug: D,
    filter: F,
) -> eyre::Result<RpcModule<()>>
where
    E: FullEthApiServer + EthApiTypes + Clone + Send + Sync + 'static,
    T: TraceApiServer + Clone + Send + Sync + 'static,
    D: DebugApiServer + Clone + Send + Sync + 'static,
    F: EthFilterApiServer<RpcTransaction<E::NetworkTypes>> + Clone + Send + Sync + 'static,
{
    let mut m = RpcModule::new(());
    let eth_clone = eth.clone();
    register_eth_get_block_by_number(&mut m, move |n, full| {
        let eth = eth_clone.clone();
        async move { eth.block_by_number(n, full).await }
    })?;

    let eth_clone = eth.clone();
    register_eth_get_transaction_by_block_number_and_index(&mut m, move |n, i| {
        let eth = eth_clone.clone();
        async move { eth.transaction_by_block_number_and_index(n, i).await }
    })?;

    let eth_clone = eth.clone();
    register_eth_get_block_transaction_count_by_number(&mut m, move |n| {
        let eth = eth_clone.clone();
        async move { eth.block_transaction_count_by_number(n).await }
    })?;

    let eth_clone = eth.clone();
    register_eth_get_uncle_count_by_block_number_via_block(&mut m, move |n, full| {
        let eth = eth_clone.clone();
        async move { eth.block_by_number(n, full).await }
    })?;

    let eth_clone = eth.clone();
    register_eth_get_block_receipts(&mut m, move |id| {
        let eth = eth_clone.clone();
        async move { EthApiServer::block_receipts(&eth, id).await }
    })?;

    let eth_clone = eth.clone();
    register_eth_get_balance(&mut m, move |addr, block| {
        let eth = eth_clone.clone();
        async move { EthApiServer::balance(&eth, addr, block).await }
    })?;

    let eth_clone = eth.clone();
    register_eth_get_code(&mut m, move |addr, block| {
        let eth = eth_clone.clone();
        async move { EthApiServer::get_code(&eth, addr, block).await }
    })?;

    let eth_clone = eth.clone();
    register_eth_get_storage_at(&mut m, move |addr, slot, block| {
        let eth = eth_clone.clone();
        async move { EthApiServer::storage_at(&eth, addr, slot, block).await }
    })?;

    let eth_clone = eth.clone();
    register_eth_get_transaction_count(&mut m, move |addr, block| {
        let eth = eth_clone.clone();
        async move {
            match EthApiServer::transaction_count(&eth, addr, block).await {
                Ok(count) => Ok(U64::from(count.to::<u64>())),
                Err(e) => Err(e),
            }
        }
    })?;

    let eth_clone = eth.clone();
    register_eth_get_proof(&mut m, move |addr, slots, block| {
        let eth = eth_clone.clone();
        async move {
            let storage_keys: Vec<JsonStorageKey> =
                slots.into_iter().map(JsonStorageKey::from).collect();
            EthApiServer::get_proof(&eth, addr, storage_keys, block).await
        }
    })?;

    let eth_clone = eth.clone();
    register_eth_call(
        &mut m,
        move |request, block, state_overrides, block_overrides| {
            let eth = eth_clone.clone();
            async move {
                EthApiServer::call(&eth, request, block, state_overrides, block_overrides).await
            }
        },
    )?;

    let eth_clone = eth.clone();
    register_eth_call_many(&mut m, move |calls, state_context, state_override| {
        let eth = eth_clone.clone();
        async move { EthApiServer::call_many(&eth, calls, state_context, state_override).await }
    })?;

    let eth_clone = eth.clone();
    register_eth_estimate_gas(&mut m, move |request, block, state_override| {
        let eth = eth_clone.clone();
        async move { EthApiServer::estimate_gas(&eth, request, block, state_override).await }
    })?;

    let eth_clone = eth.clone();
    register_eth_fee_history(
        &mut m,
        move |block_count, newest_block, reward_percentiles| {
            let eth = eth_clone.clone();
            async move {
                EthApiServer::fee_history(&eth, block_count, newest_block, reward_percentiles).await
            }
        },
    )?;

    let filter_clone = filter.clone();
    register_eth_get_logs(&mut m, move |filt| {
        let filter_api = filter_clone.clone();
        async move {
            EthFilterApiServer::<RpcTransaction<E::NetworkTypes>>::logs(&filter_api, filt).await
        }
    })?;

    let filter_clone = filter.clone();
    register_eth_new_filter(&mut m, move |filt| {
        let filter_api = filter_clone.clone();
        async move {
            EthFilterApiServer::<RpcTransaction<E::NetworkTypes>>::new_filter(&filter_api, filt)
                .await
        }
    })?;

    let trace_clone = trace.clone();
    register_trace_replay_block_transactions(&mut m, move |block_id, trace_types| {
        let trace = trace_clone.clone();
        async move { TraceApiServer::replay_block_transactions(&trace, block_id, trace_types).await }
    })?;
    let debug_clone = debug.clone();
    register_debug_trace_block_by_number(&mut m, move |block_number, opts| {
        let debug = debug_clone.clone();
        async move { DebugApiServer::debug_trace_block_by_number(&debug, block_number, opts).await }
    })?;

    let debug_clone = debug.clone();
    register_debug_trace_call(&mut m, move |request, block_number, opts| {
        let debug = debug_clone.clone();
        async move { DebugApiServer::debug_trace_call(&debug, request, block_number, opts).await }
    })?;

    let debug_clone = debug.clone();
    register_debug_trace_call_many(&mut m, move |calls, state_context, opts| {
        let debug = debug_clone.clone();
        async move { DebugApiServer::debug_trace_call_many(&debug, calls, state_context, opts).await }
    })?;
    let trace_clone = trace.clone();
    register_trace_block(&mut m, move |block_id| {
        let trace = trace_clone.clone();
        async move { TraceApiServer::trace_block(&trace, block_id).await }
    })?;

    let trace_clone = trace.clone();
    register_trace_filter(&mut m, move |trace_filter| {
        let trace = trace_clone.clone();
        async move { TraceApiServer::trace_filter(&trace, trace_filter).await }
    })?;

    let trace_clone = trace.clone();
    register_trace_call_many(&mut m, move |calls, block_id| {
        let trace = trace_clone.clone();
        async move { TraceApiServer::trace_call_many(&trace, calls, block_id).await }
    })?;
    Ok(m)
}
