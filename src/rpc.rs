use alloy_eips::BlockNumberOrTag;
use eyre::Result;
use jsonrpsee::{core::RpcResult, RpcModule};
use std::future::Future;

pub const MIN_ALLOWED_BLOCK: u64 = 1000;

fn is_block_disallowed(b: &BlockNumberOrTag) -> bool {
    match b {
        BlockNumberOrTag::Number(n) => *n < MIN_ALLOWED_BLOCK,
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
                        u64::from_str_radix(stripped, 16).ok().map(BlockNumberOrTag::Number)
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

pub fn register_eth_get_transaction_by_block_number_and_index<F, Fut, R, I>(
    m: &mut RpcModule<()>,
    tx_by_block_number_and_index: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(BlockNumberOrTag, I) -> Fut,
    I: serde::de::DeserializeOwned + Send + 'static,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("eth_getTransactionByBlockNumberAndIndex", move |params, _conn, _ctx| {
        let call = tx_by_block_number_and_index.clone();
        async move {
            let (number, index): (BlockNumberOrTag, I) = params.parse()?;
            if is_block_disallowed(&number) {
                return Ok(None);
            }
            call(number, index).await
        }
    })?;
    Ok(())
}

pub fn register_eth_get_block_transaction_count_by_number<F, Fut, R>(
    m: &mut RpcModule<()>,
    tx_count_by_block_number: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(BlockNumberOrTag) -> Fut,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("eth_getBlockTransactionCountByNumber", move |params, _conn, _ctx| {
        let call = tx_count_by_block_number.clone();
        async move {
            let (number,): (BlockNumberOrTag,) = params.parse()?;
            if is_block_disallowed(&number) {
                return Ok(None);
            }
            call(number).await
        }
    })?;
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
    m.register_async_method("eth_getUncleCountByBlockNumber", move |params, _conn, _ctx| {
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
                        .get("uncles").and_then(|a| a.as_array()).map(|a| a.len())
                        .or_else(|| v.get("ommers").and_then(|a| a.as_array()).map(|a| a.len()))
                        .unwrap_or(0);
                    Ok(Some(format!("0x{:x}", count)))
                }
                Ok(None) => Ok(None),
                Err(err) => Err(err),
            }
        }
    })?;
    Ok(())
}


pub fn register_eth_get_block_receipts<F, Fut, R>(
    m: &mut RpcModule<()>,
    get_block_receipts: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(BlockNumberOrTag) -> Fut,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("eth_getBlockReceipts", move |params, _conn, _ctx| {
        let call = get_block_receipts.clone();
        async move {
            let (number,): (BlockNumberOrTag,) = params.parse()?;
            if is_block_disallowed(&number) {
                return Ok(None);
            }
            call(number).await
        }
    })?;
    Ok(())
}

pub fn register_eth_get_balance<F, Fut, R, A>(
    m: &mut RpcModule<()>,
    get_balance: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(A, BlockNumberOrTag) -> Fut,
    A: serde::de::DeserializeOwned + Send + 'static,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("eth_getBalance", move |params, _conn, _ctx| {
        let call = get_balance.clone();
        async move {
            let (address, number): (A, BlockNumberOrTag) = params.parse()?;
            if is_block_disallowed(&number) {
                return Ok(None);
            }
            call(address, number).await
        }
    })?;
    Ok(())
}

pub fn register_eth_get_code<F, Fut, R, A>(
    m: &mut RpcModule<()>,
    get_code: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(A, BlockNumberOrTag) -> Fut,
    A: serde::de::DeserializeOwned + Send + 'static,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("eth_getCode", move |params, _conn, _ctx| {
        let call = get_code.clone();
        async move {
            let (address, number): (A, BlockNumberOrTag) = params.parse()?;
            if is_block_disallowed(&number) {
                return Ok(None);
            }
            call(address, number).await
        }
    })?;
    Ok(())
}

pub fn register_eth_get_storage_at<F, Fut, R, A, S>(
    m: &mut RpcModule<()>,
    get_storage_at: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(A, S, BlockNumberOrTag) -> Fut,
    A: serde::de::DeserializeOwned + Send + 'static,
    S: serde::de::DeserializeOwned + Send + 'static,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("eth_getStorageAt", move |params, _conn, _ctx| {
        let call = get_storage_at.clone();
        async move {
            let (address, slot, number): (A, S, BlockNumberOrTag) = params.parse()?;
            if is_block_disallowed(&number) {
                return Ok(None);
            }
            call(address, slot, number).await
        }
    })?;
    Ok(())
}

pub fn register_eth_get_transaction_count<F, Fut, R, A>(
    m: &mut RpcModule<()>,
    get_tx_count: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(A, BlockNumberOrTag) -> Fut,
    A: serde::de::DeserializeOwned + Send + 'static,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("eth_getTransactionCount", move |params, _conn, _ctx| {
        let call = get_tx_count.clone();
        async move {
            let (address, number): (A, BlockNumberOrTag) = params.parse()?;
            if is_block_disallowed(&number) {
                return Ok(None);
            }
            call(address, number).await
        }
    })?;
    Ok(())
}

pub fn register_eth_get_proof<F, Fut, R, A, Slots>(
    m: &mut RpcModule<()>,
    get_proof: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(A, Slots, BlockNumberOrTag) -> Fut,
    A: serde::de::DeserializeOwned + Send + 'static,
    Slots: serde::de::DeserializeOwned + Send + 'static,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("eth_getProof", move |params, _conn, _ctx| {
        let call = get_proof.clone();
        async move {
            let (address, slots, number): (A, Slots, BlockNumberOrTag) = params.parse()?;
            if is_block_disallowed(&number) {
                return Ok(None);
            }
            call(address, slots, number).await
        }
    })?;
    Ok(())
}

pub fn register_eth_new_filter<F, Fut, R, Filter>(
    m: &mut RpcModule<()>,
    new_filter: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(Filter) -> Fut,
    Filter: serde::de::DeserializeOwned + Send + 'static,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("eth_newFilter", move |params, _conn, _ctx| {
        let call = new_filter.clone();
        async move {
            let (fv,): (serde_json::Value,) = params.parse()?;
            if filter_has_disallowed_block_range(&fv) {
                return Ok(None);
            }
            let filter: Filter = match serde_json::from_value(fv) {
                Ok(v) => v,
                Err(_) => return Ok(None),
            };
            call(filter).await
        }
    })?;
    Ok(())
}

pub fn register_eth_call<F, Fut, R, T>(
    m: &mut RpcModule<()>,
    call_fn: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(T, BlockNumberOrTag) -> Fut,
    T: serde::de::DeserializeOwned + Send + 'static,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("eth_call", move |params, _conn, _ctx| {
        let call = call_fn.clone();
        async move {
            let (tx, number): (T, BlockNumberOrTag) = params.parse()?;
            if is_block_disallowed(&number) {
                return Ok(None);
            }
            call(tx, number).await
        }
    })?;
    Ok(())
}

pub fn register_eth_call_many<F, Fut, R, T>(
    m: &mut RpcModule<()>,
    call_many_fn: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(T, BlockNumberOrTag) -> Fut,
    T: serde::de::DeserializeOwned + Send + 'static,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("eth_callMany", move |params, _conn, _ctx| {
        let call = call_many_fn.clone();
        async move {
            let (calls, number): (T, BlockNumberOrTag) = params.parse()?;
            if is_block_disallowed(&number) {
                return Ok(None);
            }
            call(calls, number).await
        }
    })?;
    Ok(())
}

pub fn register_eth_estimate_gas<F, Fut, R, T>(
    m: &mut RpcModule<()>,
    estimate_fn: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(T, BlockNumberOrTag) -> Fut,
    T: serde::de::DeserializeOwned + Send + 'static,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("eth_estimateGas", move |params, _conn, _ctx| {
        let call = estimate_fn.clone();
        async move {
            let (tx, number): (T, BlockNumberOrTag) = params.parse()?;
            if is_block_disallowed(&number) {
                return Ok(None);
            }
            call(tx, number).await
        }
    })?;
    Ok(())
}

pub fn register_eth_fee_history<F, Fut, R, Count, Reward>(
    m: &mut RpcModule<()>,
    fee_history_fn: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(Count, BlockNumberOrTag, Reward) -> Fut,
    Count: serde::de::DeserializeOwned + Send + 'static,
    Reward: serde::de::DeserializeOwned + Send + 'static,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("eth_feeHistory", move |params, _conn, _ctx| {
        let call = fee_history_fn.clone();
        async move {
            let (count, number, reward): (Count, BlockNumberOrTag, Reward) = params.parse()?;
            if is_block_disallowed(&number) {
                return Ok(None);
            }
            call(count, number, reward).await
        }
    })?;
    Ok(())
}

pub fn register_eth_get_logs<F, Fut, R, Filter>(
    m: &mut RpcModule<()>,
    get_logs_fn: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(Filter) -> Fut,
    Filter: serde::de::DeserializeOwned + Send + 'static,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("eth_getLogs", move |params, _conn, _ctx| {
        let call = get_logs_fn.clone();
        async move {
            let (fv,): (serde_json::Value,) = params.parse()?;
            if filter_has_disallowed_block_range(&fv) {
                return Ok(None);
            }
            let filter: Filter = match serde_json::from_value(fv) {
                Ok(v) => v,
                Err(_) => return Ok(None),
            };
            call(filter).await
        }
    })?;
    Ok(())
}

// --- Trace helpers (generic over types) ---

pub fn register_trace_replay_block_transactions<F, Fut, R, Types>(
    m: &mut RpcModule<()>,
    replay_fn: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(BlockNumberOrTag, Types) -> Fut,
    Types: serde::de::DeserializeOwned + Send + 'static,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("trace_replayBlockTransactions", move |params, _conn, _ctx| {
        let call = replay_fn.clone();
        async move {
            let (number, types): (BlockNumberOrTag, Types) = params.parse()?;
            if is_block_disallowed(&number) {
                return Ok(None);
            }
            call(number, types).await
        }
    })?;
    Ok(())
}

// --- Debug helpers ---

pub fn register_debug_trace_block_by_number<F, Fut, R, Opts>(
    m: &mut RpcModule<()>,
    trace_fn: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(BlockNumberOrTag, Opts) -> Fut,
    Opts: serde::de::DeserializeOwned + Send + 'static,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("debug_traceBlockByNumber", move |params, _conn, _ctx| {
        let call = trace_fn.clone();
        async move {
            let (number, opts): (BlockNumberOrTag, Opts) = params.parse()?;
            if is_block_disallowed(&number) {
                return Ok(None);
            }
            call(number, opts).await
        }
    })?;
    Ok(())
}

pub fn register_debug_trace_call<F, Fut, R, Tx, Opts>(
    m: &mut RpcModule<()>,
    trace_call_fn: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(Tx, Opts, BlockNumberOrTag) -> Fut,
    Tx: serde::de::DeserializeOwned + Send + 'static,
    Opts: serde::de::DeserializeOwned + Send + 'static,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("debug_traceCall", move |params, _conn, _ctx| {
        let call = trace_call_fn.clone();
        async move {
            let (tx, opts, number): (Tx, Opts, BlockNumberOrTag) = params.parse()?;
            if is_block_disallowed(&number) {
                return Ok(None);
            }
            call(tx, opts, number).await
        }
    })?;
    Ok(())
}

pub fn register_debug_storage_range_at<F, Fut, R, A, B, C, D>(
    m: &mut RpcModule<()>,
    storage_range_fn: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(BlockNumberOrTag, A, B, C, D) -> Fut,
    A: serde::de::DeserializeOwned + Send + 'static,
    B: serde::de::DeserializeOwned + Send + 'static,
    C: serde::de::DeserializeOwned + Send + 'static,
    D: serde::de::DeserializeOwned + Send + 'static,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("debug_storageRangeAt", move |params, _conn, _ctx| {
        let call = storage_range_fn.clone();
        async move {
            let (number, a, b, c, d): (BlockNumberOrTag, A, B, C, D) = params.parse()?;
            if is_block_disallowed(&number) {
                return Ok(None);
            }
            call(number, a, b, c, d).await
        }
    })?;
    Ok(())
}

pub fn register_debug_trace_call_many<F, Fut, R, Calls, Opts>(
    m: &mut RpcModule<()>,
    trace_call_many_fn: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(Calls, Opts, BlockNumberOrTag) -> Fut,
    Calls: serde::de::DeserializeOwned + Send + 'static,
    Opts: serde::de::DeserializeOwned + Send + 'static,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("debug_traceCallMany", move |params, _conn, _ctx| {
        let call = trace_call_many_fn.clone();
        async move {
            let (calls, opts, number): (Calls, Opts, BlockNumberOrTag) = params.parse()?;
            if is_block_disallowed(&number) {
                return Ok(None);
            }
            call(calls, opts, number).await
        }
    })?;
    Ok(())
}

pub fn register_trace_block<F, Fut, R>(
    m: &mut RpcModule<()>,
    block_fn: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(BlockNumberOrTag) -> Fut,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("trace_block", move |params, _conn, _ctx| {
        let call = block_fn.clone();
        async move {
            let (number,): (BlockNumberOrTag,) = params.parse()?;
            if is_block_disallowed(&number) {
                return Ok(None);
            }
            call(number).await
        }
    })?;
    Ok(())
}

pub fn register_trace_filter<F, Fut, R, TF>(
    m: &mut RpcModule<()>,
    filter_fn: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(TF) -> Fut,
    TF: serde::de::DeserializeOwned + Send + 'static,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("trace_filter", move |params, _conn, _ctx| {
        let call = filter_fn.clone();
        async move {
            let (fv,): (serde_json::Value,) = params.parse()?;
            if filter_has_disallowed_block_range(&fv) {
                return Ok(None);
            }
            let tf: TF = match serde_json::from_value(fv) {
                Ok(v) => v,
                Err(_) => return Ok(None),
            };
            call(tf).await
        }
    })?;
    Ok(())
}

pub fn register_trace_call_many<F, Fut, R, T>(
    m: &mut RpcModule<()>,
    call_many_fn: F,
) -> Result<()>
where
    F: Clone + Send + Sync + 'static + Fn(T, BlockNumberOrTag) -> Fut,
    T: serde::de::DeserializeOwned + Send + 'static,
    Fut: Future<Output = RpcResult<Option<R>>> + Send + 'static,
    R: serde::Serialize + Clone + Send + 'static,
{
    m.register_async_method("trace_callMany", move |params, _conn, _ctx| {
        let call = call_many_fn.clone();
        async move {
            let (calls, number): (T, BlockNumberOrTag) = params.parse()?;
            if is_block_disallowed(&number) {
                return Ok(None);
            }
            call(calls, number).await
        }
    })?;
    Ok(())
}


pub fn install_all_with_full<
    // core eth
    B, Bfut, Br,
    Tx, Txfut, Txr, I,
    C, Cfut, Cr,
    // additional eth
    Rcp, Rcpfut, Rcpr,
    Bal, Balfut, Balr, Addr1,
    Code, Codefut, Coder, Addr2,
    Sto, Stofut, Stor, Addr3, Slot,
    TxCnt, TxCntfut, TxCntr, Addr4,
    Proof, Prooffut, Proofr, Addr5, Slots,
    NewF, NewFfut, NewFr, Filter1,
    Call, Callfut, Callr, TxCall,
    CallM, CallMfut, CallMr, Calls1,
    Est, Estfut, Estr, TxEst,
    Fee, Feefut, Feer, Count, Reward,
    Logs, Logsfut, Logsr, Filter2,
    // trace
    TrRep, TrRepfut, TrRepr, TrTypes,
    TrBlk, TrBlkfut, TrBlkr,
    TrFilt, TrFilfut, TrFilr, TrFilter,
    TrCallM, TrCallMfut, TrCallMr, TrCalls,
    // debug
    DBlk, DBlkfut, DBlkr, DOpts1,
    DCall, DCallfut, DCallr, DTx, DOpts2,
    DStor, DStorfut, DStorr, DA, DB, DC, DD,
    DCallM, DCallMfut, DCallMr, DCalls, DOpts3,
>(
    // core eth
    block_by_number: B,
    tx_by_block_number_and_index: Tx,
    block_tx_count_by_number: C,
    // additional eth
    get_block_receipts: Rcp,
    get_balance: Bal,
    get_code: Code,
    get_storage_at: Sto,
    get_tx_count: TxCnt,
    get_proof: Proof,
    new_filter: NewF,
    call_fn: Call,
    call_many_fn: CallM,
    estimate_gas_fn: Est,
    fee_history_fn: Fee,
    get_logs_fn: Logs,
    // trace
    trace_replay_fn: TrRep,
    trace_block_fn: TrBlk,
    trace_filter_fn: TrFilt,
    trace_call_many_fn: TrCallM,
    // debug
    debug_trace_block_fn: DBlk,
    debug_trace_call_fn: DCall,
    debug_storage_range_at_fn: DStor,
    debug_trace_call_many_fn: DCallM,
) -> Result<RpcModule<()>>
where
    // core eth
    B: Clone + Send + Sync + 'static + Fn(BlockNumberOrTag, bool) -> Bfut,
    Bfut: Future<Output = RpcResult<Option<Br>>> + Send + 'static,
    Br: serde::Serialize + Clone + Send + 'static,
    Tx: Clone + Send + Sync + 'static + Fn(BlockNumberOrTag, I) -> Txfut,
    I: serde::de::DeserializeOwned + Send + 'static,
    Txfut: Future<Output = RpcResult<Option<Txr>>> + Send + 'static,
    Txr: serde::Serialize + Clone + Send + 'static,
    C: Clone + Send + Sync + 'static + Fn(BlockNumberOrTag) -> Cfut,
    Cfut: Future<Output = RpcResult<Option<Cr>>> + Send + 'static,
    Cr: serde::Serialize + Clone + Send + 'static,
    // additional eth
    Rcp: Clone + Send + Sync + 'static + Fn(BlockNumberOrTag) -> Rcpfut,
    Rcpfut: Future<Output = RpcResult<Option<Rcpr>>> + Send + 'static,
    Rcpr: serde::Serialize + Clone + Send + 'static,
    Bal: Clone + Send + Sync + 'static + Fn(Addr1, BlockNumberOrTag) -> Balfut,
    Addr1: serde::de::DeserializeOwned + Send + 'static,
    Balfut: Future<Output = RpcResult<Option<Balr>>> + Send + 'static,
    Balr: serde::Serialize + Clone + Send + 'static,
    Code: Clone + Send + Sync + 'static + Fn(Addr2, BlockNumberOrTag) -> Codefut,
    Addr2: serde::de::DeserializeOwned + Send + 'static,
    Codefut: Future<Output = RpcResult<Option<Coder>>> + Send + 'static,
    Coder: serde::Serialize + Clone + Send + 'static,
    Sto: Clone + Send + Sync + 'static + Fn(Addr3, Slot, BlockNumberOrTag) -> Stofut,
    Addr3: serde::de::DeserializeOwned + Send + 'static,
    Slot: serde::de::DeserializeOwned + Send + 'static,
    Stofut: Future<Output = RpcResult<Option<Stor>>> + Send + 'static,
    Stor: serde::Serialize + Clone + Send + 'static,
    TxCnt: Clone + Send + Sync + 'static + Fn(Addr4, BlockNumberOrTag) -> TxCntfut,
    Addr4: serde::de::DeserializeOwned + Send + 'static,
    TxCntfut: Future<Output = RpcResult<Option<TxCntr>>> + Send + 'static,
    TxCntr: serde::Serialize + Clone + Send + 'static,
    Proof: Clone + Send + Sync + 'static + Fn(Addr5, Slots, BlockNumberOrTag) -> Prooffut,
    Addr5: serde::de::DeserializeOwned + Send + 'static,
    Slots: serde::de::DeserializeOwned + Send + 'static,
    Prooffut: Future<Output = RpcResult<Option<Proofr>>> + Send + 'static,
    Proofr: serde::Serialize + Clone + Send + 'static,
    NewF: Clone + Send + Sync + 'static + Fn(Filter1) -> NewFfut,
    Filter1: serde::de::DeserializeOwned + Send + 'static,
    NewFfut: Future<Output = RpcResult<Option<NewFr>>> + Send + 'static,
    NewFr: serde::Serialize + Clone + Send + 'static,
    Call: Clone + Send + Sync + 'static + Fn(TxCall, BlockNumberOrTag) -> Callfut,
    TxCall: serde::de::DeserializeOwned + Send + 'static,
    Callfut: Future<Output = RpcResult<Option<Callr>>> + Send + 'static,
    Callr: serde::Serialize + Clone + Send + 'static,
    CallM: Clone + Send + Sync + 'static + Fn(Calls1, BlockNumberOrTag) -> CallMfut,
    Calls1: serde::de::DeserializeOwned + Send + 'static,
    CallMfut: Future<Output = RpcResult<Option<CallMr>>> + Send + 'static,
    CallMr: serde::Serialize + Clone + Send + 'static,
    Est: Clone + Send + Sync + 'static + Fn(TxEst, BlockNumberOrTag) -> Estfut,
    TxEst: serde::de::DeserializeOwned + Send + 'static,
    Estfut: Future<Output = RpcResult<Option<Estr>>> + Send + 'static,
    Estr: serde::Serialize + Clone + Send + 'static,
    Fee: Clone + Send + Sync + 'static + Fn(Count, BlockNumberOrTag, Reward) -> Feefut,
    Count: serde::de::DeserializeOwned + Send + 'static,
    Reward: serde::de::DeserializeOwned + Send + 'static,
    Feefut: Future<Output = RpcResult<Option<Feer>>> + Send + 'static,
    Feer: serde::Serialize + Clone + Send + 'static,
    Logs: Clone + Send + Sync + 'static + Fn(Filter2) -> Logsfut,
    Filter2: serde::de::DeserializeOwned + Send + 'static,
    Logsfut: Future<Output = RpcResult<Option<Logsr>>> + Send + 'static,
    Logsr: serde::Serialize + Clone + Send + 'static,
    // trace
    TrRep: Clone + Send + Sync + 'static + Fn(BlockNumberOrTag, TrTypes) -> TrRepfut,
    TrTypes: serde::de::DeserializeOwned + Send + 'static,
    TrRepfut: Future<Output = RpcResult<Option<TrRepr>>> + Send + 'static,
    TrRepr: serde::Serialize + Clone + Send + 'static,
    TrBlk: Clone + Send + Sync + 'static + Fn(BlockNumberOrTag) -> TrBlkfut,
    TrBlkfut: Future<Output = RpcResult<Option<TrBlkr>>> + Send + 'static,
    TrBlkr: serde::Serialize + Clone + Send + 'static,
    TrFilt: Clone + Send + Sync + 'static + Fn(TrFilter) -> TrFilfut,
    TrFilter: serde::de::DeserializeOwned + Send + 'static,
    TrFilfut: Future<Output = RpcResult<Option<TrFilr>>> + Send + 'static,
    TrFilr: serde::Serialize + Clone + Send + 'static,
    TrCallM: Clone + Send + Sync + 'static + Fn(TrCalls, BlockNumberOrTag) -> TrCallMfut,
    TrCalls: serde::de::DeserializeOwned + Send + 'static,
    TrCallMfut: Future<Output = RpcResult<Option<TrCallMr>>> + Send + 'static,
    TrCallMr: serde::Serialize + Clone + Send + 'static,
    // debug
    DBlk: Clone + Send + Sync + 'static + Fn(BlockNumberOrTag, DOpts1) -> DBlkfut,
    DOpts1: serde::de::DeserializeOwned + Send + 'static,
    DBlkfut: Future<Output = RpcResult<Option<DBlkr>>> + Send + 'static,
    DBlkr: serde::Serialize + Clone + Send + 'static,
    DCall: Clone + Send + Sync + 'static + Fn(DTx, DOpts2, BlockNumberOrTag) -> DCallfut,
    DTx: serde::de::DeserializeOwned + Send + 'static,
    DOpts2: serde::de::DeserializeOwned + Send + 'static,
    DCallfut: Future<Output = RpcResult<Option<DCallr>>> + Send + 'static,
    DCallr: serde::Serialize + Clone + Send + 'static,
    DStor: Clone + Send + Sync + 'static + Fn(BlockNumberOrTag, DA, DB, DC, DD) -> DStorfut,
    DA: serde::de::DeserializeOwned + Send + 'static,
    DB: serde::de::DeserializeOwned + Send + 'static,
    DC: serde::de::DeserializeOwned + Send + 'static,
    DD: serde::de::DeserializeOwned + Send + 'static,
    DStorfut: Future<Output = RpcResult<Option<DStorr>>> + Send + 'static,
    DStorr: serde::Serialize + Clone + Send + 'static,
    DCallM: Clone + Send + Sync + 'static + Fn(DCalls, DOpts3, BlockNumberOrTag) -> DCallMfut,
    DCalls: serde::de::DeserializeOwned + Send + 'static,
    DOpts3: serde::de::DeserializeOwned + Send + 'static,
    DCallMfut: Future<Output = RpcResult<Option<DCallMr>>> + Send + 'static,
    DCallMr: serde::Serialize + Clone + Send + 'static,
{
    let mut m = RpcModule::new(());
    let block_by_number_for_uncles = block_by_number.clone();
    register_eth_get_block_by_number(&mut m, block_by_number)?;
    register_eth_get_transaction_by_block_number_and_index(&mut m, tx_by_block_number_and_index)?;
    register_eth_get_block_transaction_count_by_number(&mut m, block_tx_count_by_number)?;
    register_eth_get_uncle_count_by_block_number_via_block(&mut m, block_by_number_for_uncles)?;

    register_eth_get_block_receipts(&mut m, get_block_receipts)?;
    register_eth_get_balance(&mut m, get_balance)?;
    register_eth_get_code(&mut m, get_code)?;
    register_eth_get_storage_at(&mut m, get_storage_at)?;
    register_eth_get_transaction_count(&mut m, get_tx_count)?;
    register_eth_get_proof(&mut m, get_proof)?;
    register_eth_new_filter(&mut m, new_filter)?;
    register_eth_call(&mut m, call_fn)?;
    register_eth_call_many(&mut m, call_many_fn)?;
    register_eth_estimate_gas(&mut m, estimate_gas_fn)?;
    register_eth_fee_history(&mut m, fee_history_fn)?;
    register_eth_get_logs(&mut m, get_logs_fn)?;

    register_trace_replay_block_transactions(&mut m, trace_replay_fn)?;
    register_trace_block(&mut m, trace_block_fn)?;
    register_trace_filter(&mut m, trace_filter_fn)?;
    register_trace_call_many(&mut m, trace_call_many_fn)?;

    register_debug_trace_block_by_number(&mut m, debug_trace_block_fn)?;
    register_debug_trace_call(&mut m, debug_trace_call_fn)?;
    register_debug_storage_range_at(&mut m, debug_storage_range_at_fn)?;
    register_debug_trace_call_many(&mut m, debug_trace_call_many_fn)?;

    Ok(m)
}

pub fn build_guarded_module<E, T, D>(_eth: E, _trace: T, _debug: D) -> eyre::Result<RpcModule<()>>
where
    E: Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    D: Clone + Send + Sync + 'static,
{
    install_all_with_full(
        // core eth as no-ops (guards still apply)
        move |_n: alloy_eips::BlockNumberOrTag, _f: bool| async move { Ok(None::<serde_json::Value>) },
        move |_n: alloy_eips::BlockNumberOrTag, _i: serde_json::Value| async move { Ok(None::<serde_json::Value>) },
        move |_n: alloy_eips::BlockNumberOrTag| async move { Ok(None::<serde_json::Value>) },
        // additional eth as no-ops (guards still apply)
        move |_n: alloy_eips::BlockNumberOrTag| async move { Ok(None::<serde_json::Value>) },
        move |_addr: serde_json::Value, _n: alloy_eips::BlockNumberOrTag| async move { Ok(None::<serde_json::Value>) },
        move |_addr: serde_json::Value, _n: alloy_eips::BlockNumberOrTag| async move { Ok(None::<serde_json::Value>) },
        move |_addr: serde_json::Value, _slot: serde_json::Value, _n: alloy_eips::BlockNumberOrTag| async move { Ok(None::<serde_json::Value>) },
        move |_addr: serde_json::Value, _n: alloy_eips::BlockNumberOrTag| async move { Ok(None::<serde_json::Value>) },
        move |_addr: serde_json::Value, _slots: serde_json::Value, _n: alloy_eips::BlockNumberOrTag| async move { Ok(None::<serde_json::Value>) },
        move |_filter: serde_json::Value| async move { Ok(None::<serde_json::Value>) },
        move |_tx: serde_json::Value, _n: alloy_eips::BlockNumberOrTag| async move { Ok(None::<serde_json::Value>) },
        move |_calls: serde_json::Value, _n: alloy_eips::BlockNumberOrTag| async move { Ok(None::<serde_json::Value>) },
        move |_tx: serde_json::Value, _n: alloy_eips::BlockNumberOrTag| async move { Ok(None::<serde_json::Value>) },
        move |_count: serde_json::Value, _n: alloy_eips::BlockNumberOrTag, _reward: serde_json::Value| async move { Ok(None::<serde_json::Value>) },
        move |_filter: serde_json::Value| async move { Ok(None::<serde_json::Value>) },
        // trace as no-ops
        move |_n: alloy_eips::BlockNumberOrTag, _types: serde_json::Value| async move { Ok(None::<serde_json::Value>) },
        move |_n: alloy_eips::BlockNumberOrTag| async move { Ok(None::<serde_json::Value>) },
        move |_tf: serde_json::Value| async move { Ok(None::<serde_json::Value>) },
        move |_calls: serde_json::Value, _n: alloy_eips::BlockNumberOrTag| async move { Ok(None::<serde_json::Value>) },
        // debug as no-ops
        move |_n: alloy_eips::BlockNumberOrTag, _opts: serde_json::Value| async move { Ok(None::<serde_json::Value>) },
        move |_tx: serde_json::Value, _opts: serde_json::Value, _n: alloy_eips::BlockNumberOrTag| async move { Ok(None::<serde_json::Value>) },
        move |_n: alloy_eips::BlockNumberOrTag, _a: serde_json::Value, _b: serde_json::Value, _c: serde_json::Value, _d: serde_json::Value| async move { Ok(None::<serde_json::Value>) },
        move |_calls: serde_json::Value, _opts: serde_json::Value, _n: alloy_eips::BlockNumberOrTag| async move { Ok(None::<serde_json::Value>) },
    )
}

