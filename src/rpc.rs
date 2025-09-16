use std::future::Future;

use alloy_eips::{BlockId, BlockNumberOrTag};
use alloy_primitives::BlockNumber;

use jsonrpsee::{
    core::middleware::{Batch, Notification, RpcServiceT},
    server::MethodResponse as RpcMethodResponse,
    types::{Params, Request, ResponsePayload},
};
use serde_json::Value;

use tower::Layer;
use tracing::debug;

#[derive(Debug, Clone, Copy)]
pub struct BlockFloorLayer {
    min_block: BlockNumber,
}

impl BlockFloorLayer {
    pub const fn with_min_block(min_block: BlockNumber) -> Self { Self { min_block } }

    // Chiado (10200) -> 700000, Gnosis (100) -> 26478650, Others -> 0
    pub const fn from_chain_id(chain_id: u64) -> Self {
        match chain_id {
            10200 => Self { min_block: 700_000 },
            100 => Self { min_block: 26_478_650 },
            _ => Self { min_block: 0 },
        }
    }

    // Reads chain id from env var set by main and creates the appropriate layer.
    // Falls back to 0 (no floor) if unset or invalid.
    pub fn from_env() -> Self {
        let chain_id = std::env::var("RETH_GNOSIS_CHAIN_ID")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        Self::from_chain_id(chain_id)
    }
}

// The Layer produces a Service that implements RpcServiceT.
impl<S> Layer<S> for BlockFloorLayer {
    type Service = BlockFloorService<S>;

    fn layer(&self, inner: S) -> Self::Service { BlockFloorService { inner, min_block: self.min_block } }
}

#[derive(Debug, Clone)]
pub struct BlockFloorService<S> { inner: S, min_block: BlockNumber }

impl<S> RpcServiceT for BlockFloorService<S>
where
    S: RpcServiceT<MethodResponse = RpcMethodResponse> + Send + Sync + Clone + 'static,
{
    type MethodResponse = S::MethodResponse;
    type NotificationResponse = S::NotificationResponse;
    type BatchResponse = S::BatchResponse;

    fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        let next = self.inner.clone();
        let min_block = self.min_block;

        Box::pin(async move {
            // Intercept a curated set of methods and return `null` if the target block is below MIN_BLOCK
            if should_block_by_method(req.method_name(), &req.params(), min_block) {
                let payload = ResponsePayload::success(serde_json::Value::Null).into();
                return RpcMethodResponse::response(req.id.clone(), payload, usize::MAX)
            }

            // Default path
            next.call(req).await
        })
    }

    fn batch<'a>(&self, req: Batch<'a>) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        self.inner.batch(req)
    }

    fn notification<'a>(
        &self,
        n: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
        self.inner.notification(n)
    }
}

fn should_block_by_method(
    method: &str,
    params: &Params<'_>,
    min_block: BlockNumber,
) -> bool {
    let mut block_below = |bid: BlockId| -> bool {
        match bid {
            BlockId::Number(BlockNumberOrTag::Number(n)) => n < min_block,
            _ => {
                // Tags or hashes: allow through (we only enforce explicit numbers here)
                debug!(target: "rpc::block_floor", method=%method, "non-numeric BlockId; allowing");
                false
            }
        }
    };

    // Methods that carry a single BlockId at a fixed position
    let single_pos = |pos: usize| -> bool {
        parse_block_id_from_params(params, pos).map(|b| block_below(b)).unwrap_or(false)
    };

    match method {
        "eth_getBlockByNumber" |
        "eth_getTransactionByBlockNumberAndIndex" |
        "eth_getBlockTransactionCountByNumber" |
        "eth_getUncleCountByBlockNumber" |
        "eth_getBlockReceipts" |
        "trace_replayBlockTransactions" |
        "trace_block" |
        "debug_traceBlockByNumber" => return single_pos(0),
        "eth_getBalance" |
        "eth_getCode" |
        "eth_getTransactionCount" |
        "eth_call" |
        "eth_estimateGas" |
        "debug_traceCall" |
        "debug_traceCallMany" => return single_pos(1),    
        "eth_getStorageAt" |
        "eth_getProof" => return single_pos(2),
        "eth_feeHistory" => return single_pos(1),
        "eth_callMany" | "trace_callMany" => return single_pos(1),
        "eth_newFilter" | "eth_getLogs" | "trace_filter" => {
            return filter_has_block_below_floor(params, &mut block_below)
        }

        _ => {}
    }

    false
}

/// Parse a BlockId from params at a position.
fn parse_block_id_from_params(params: &Params<'_>, pos: usize) -> Option<BlockId> {
    let values: Vec<Value> = params.parse().ok()?;
    let v = values.into_iter().nth(pos)?;
    serde_json::from_value::<BlockId>(v).ok()
}

fn filter_has_block_below_floor(
    params: &Params<'_>,
    block_below: &mut dyn FnMut(BlockId) -> bool,
) -> bool {
    let values: Vec<Value> = match params.parse() {
        Ok(v) => v,
        Err(_) => return false,
    };
    let Some(obj) = values.into_iter().nth(0) else { return false };

    // Try to read "fromBlock" and "toBlock" if present
    let from_below = obj.get("fromBlock")
        .and_then(|v| serde_json::from_value::<BlockId>(v.clone()).ok())
        .map(|bid| block_below(bid))
        .unwrap_or(false);

    if from_below { return true }

    let to_below = obj.get("toBlock")
        .and_then(|v| serde_json::from_value::<BlockId>(v.clone()).ok())
        .map(|bid| block_below(bid))
        .unwrap_or(false);

    to_below
}


use gnosis_primitives::header::GnosisHeader;
use reth_rpc::RpcTypes;

/// The gnosis RPC network types
#[derive(Debug, Copy, Default, Clone)]
#[non_exhaustive]
pub struct GnosisNetwork;

impl RpcTypes for GnosisNetwork {
    type Header = alloy_rpc_types_eth::Header<GnosisHeader>;
    type Receipt = alloy_rpc_types_eth::TransactionReceipt;
    type TransactionRequest = alloy_rpc_types_eth::transaction::TransactionRequest;
    type TransactionResponse = alloy_rpc_types_eth::Transaction;
}
