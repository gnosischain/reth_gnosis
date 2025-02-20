#!/bin/bash
set -e

# Script's directory
DIR="$(dirname "$0")"
# 
# "$DIR/run_reth.sh" &
# Start the container 
docker run --name neth-vec-gen --rm \
  -v $DIR/networkdata:/networkdata \
  -p 8545:8545 \
  -p 8546:8546 \
  nethermind/nethermind:latest \
  --config=none \
  --Init.ChainSpecPath=/networkdata/chainspec.json \
  --Init.DiscoveryEnabled=false \
  --JsonRpc.Enabled=true \
  --JsonRpc.Host=0.0.0.0 \
  --JsonRpc.Port=8545 \
  --JsonRpc.EngineHost=0.0.0.0 \
  --JsonRpc.EnginePort=8546 \
  --JsonRpc.JwtSecretFile=/networkdata/jwtsecret \
  --TraceStore.Enabled=true &
BG_PID=$!

OUT_DIR=$DIR/test_blocks
mkdir -p $OUT_DIR

# Set the trap to call cleanup if an error occurs
cleanup() {
  echo "Stopping node process (PID: $BG_PID)..."
  kill $BG_PID 2>/dev/null || true
  docker rm -f neth-vec-gen 2>/dev/null || true
  # TODO: REMOVE THIS
  pkill -f "reth node" || true
}
trap cleanup EXIT

# Retry the curl command until it succeeds
until curl -X POST -H "Content-Type: application/json" \
  --data '{"jsonrpc":"2.0","method":"eth_getBlockByNumber","params":["0x0", false],"id":1}' \
  http://localhost:8545; do
    echo "Retrying..."
    sleep 2
done

echo "EL is available"

# The ASCII representation of `2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a`
JWT_SECRET="********************************"
# Generate a JWT token using the secret key
# jwt is this CLI tool https://github.com/mike-engel/jwt-cli/tree/main
# iat is appended automatically
JWT_TOKEN=$(jwt encode --alg HS256 --secret "$JWT_SECRET")
echo JWT_TOKEN: $JWT_TOKEN

declare -i BLOCK_COUNTER=1

echo "Making block $BLOCK_COUNTER"

sleep 3

##########################################
##### making an extra block at first #####
##########################################
TIMESTAMP=1850000000
HEAD_BLOCK=$(curl -X POST -H "Content-Type: application/json" \
    --data "{
        \"jsonrpc\":\"2.0\",
        \"method\":\"eth_getBlockByNumber\",
        \"params\":[
        \"latest\",
        false
        ],
        \"id\":1
    }" \
    http://localhost:8545 \
)

HEAD_BLOCK_HASH=$(echo $HEAD_BLOCK | jq --raw-output '.result.hash')
echo HEAD_BLOCK_HASH=$HEAD_BLOCK_HASH
RESPONSE=$(curl -X POST -H "Content-Type: application/json" \
    -H "Authorization: Bearer $JWT_TOKEN" \
    --data "{
        \"jsonrpc\":\"2.0\",
        \"method\":\"engine_forkchoiceUpdatedV3\",
        \"params\":[
        {
            \"headBlockHash\": \"$HEAD_BLOCK_HASH\",
            \"safeBlockHash\": \"0x0000000000000000000000000000000000000000000000000000000000000000\",
            \"finalizedBlockHash\": \"0x0000000000000000000000000000000000000000000000000000000000000000\"
        },
        {
            \"timestamp\": $TIMESTAMP,
            \"prevRandao\": \"0x0000000000000000000000000000000000000000000000000000000000000000\",
            \"suggestedFeeRecipient\": \"0x0000000000000000000000000000000000000000\",
            \"withdrawals\": [],
            \"parentBeaconBlockRoot\": \"0x1100000000000000000000000000000000000000000000000000000000000000\"
        }
        ],
        \"id\":1
    }" \
    http://localhost:8546 \
)
echo engine_forkchoiceUpdatedV3 trigger block production RESPONSE $RESPONSE

PAYLOAD_ID=$(echo $RESPONSE | jq --raw-output '.result.payloadId')
echo PAYLOAD_ID=$PAYLOAD_ID

echo "Sending transaction on block $BLOCK_COUNTER to create deposit contract"

# RLP encoded form of the following transaction:
# transaction = {
#     'from': "0x38e3E7Aca6762E296F659Fcb4E460a3A621dcD3D",
#     'value': 0,
#     'nonce': 0,
#     'gas': 3836885,
#     'gasPrice': 2000000000,
#     'data': deposit_contract_bytecode,
# }
# where 'deposit_contract_bytecode' is the input data from https://gnosisscan.io/tx/0x2ca31c57363a9950c8124266f003bc7f0e5f30772028476e8de357b713ff5da3

RESPONSE=$(curl -X POST -H "Content-Type: application/json" \
    -H "Authorization: Bearer $JWT_TOKEN" \
    --data '{"jsonrpc":"2.0","method":"eth_sendRawTransaction","params":["f8bb808477359400833a8bd58080b86a60618060095f395ff33373fffffffffffffffffffffffffffffffffffffffe14604d57602036146024575f5ffd5b5f35801560495762001fff810690815414603c575f5ffd5b62001fff01545f5260205ff35b5f5ffd5b62001fff42064281555f359062001fff0155001ba0e150dfe4eb457e2be80add404502c646bb0f371951d21fb1ed59e9095d78c447a06872491f5d6920fa66aab23bc19c5c6511d5227735f30fd91f78d7f186799a96"],"id":1}' \
    http://localhost:8546 \
)

echo eth_sendRawTransaction RESPONSE $RESPONSE
TX1HASH=$(echo $RESPONSE | jq --raw-output '.result')
echo TX1HASH=$TX1HASH

# exit if the transaction is not sent
if [ "$TX1HASH" == "null" ]; then
  echo "Transaction not sent"
  docker rm -f neth-vec-gen 2>/dev/null
  exit 1
fi

echo "Transaction sent"