#!/bin/bash
set -e

# Script's directory
DIR="$(dirname "$0")"

sleep 3

# "$DIR/run_reth.sh" &
$DIR/run_nethermind.sh &
BG_PID=$!

OUT_DIR=$DIR/eip1559_blocks
mkdir -p $OUT_DIR

# Set the trap to call cleanup if an error occurs
cleanup() {
  echo "Stopping node process (PID: $BG_PID)..."
  ps aux | grep "reth node" | grep -v grep | awk '{print $2}' | xargs kill
  kill $BG_PID 2>/dev/null || true
  docker rm -f neth-vec-gen 2>/dev/null || true
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

declare -i BLOCK_COUNTER=1

echo "Making block $BLOCK_COUNTER"

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

# The ASCII representation of `2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a`
JWT_SECRET="********************************"
# Generate a JWT token using the secret key
# jwt is this CLI tool https://github.com/mike-engel/jwt-cli/tree/main
# iat is appended automatically
JWT_TOKEN=$(jwt encode --alg HS256 --secret "$JWT_SECRET")
echo JWT_TOKEN: $JWT_TOKEN

TIMESTAMP=$((1700000000 + BLOCK_COUNTER))

# Request to produce block on current head

RESPONSE=$(curl -X POST -H "Content-Type: application/json" \
    -H "Authorization: Bearer $JWT_TOKEN" \
    --data "{
        \"jsonrpc\":\"2.0\",
        \"method\":\"engine_forkchoiceUpdatedV1\",
        \"params\":[
        {
            \"headBlockHash\": \"$HEAD_BLOCK_HASH\",
            \"safeBlockHash\": \"0x0000000000000000000000000000000000000000000000000000000000000000\",
            \"finalizedBlockHash\": \"0x0000000000000000000000000000000000000000000000000000000000000000\"
        },
        {
            \"timestamp\": $TIMESTAMP,
            \"prevRandao\": \"0x0000000000000000000000000000000000000000000000000000000000000000\",
            \"suggestedFeeRecipient\": \"0x0000000000000000000000000000000000000000\"
        }
        ],
        \"id\":1
    }" \
    http://localhost:8546 \
)
echo engine_forkchoiceUpdatedV1 trigger block production RESPONSE $RESPONSE

PAYLOAD_ID=$(echo $RESPONSE | jq --raw-output '.result.payloadId')
echo PAYLOAD_ID=$PAYLOAD_ID

echo "Sending transaction on block $BLOCK_COUNTER"

# sending RLP encoded form of:
# transaction = {
#     'from': "0x38e3E7Aca6762E296F659Fcb4E460a3A621dcD3D",
#     'to': "0xb42a8c62f3278AFc9343A8FcCD5232CBe8aA5117",
#     'value': 1100000000,
#     'nonce': 0,
#     'gas': 200000,
#     'maxFeePerGas': 2500000000,
#     'maxPriorityFeePerGas': 2500000000,
#     'chainId': 10200
# }
# signed using pvt key: 0x000038e28d32db8e509354d6b359eb58646e84bc942e3c79f470b08ebc976e1c

RESPONSE=$(curl -X POST -H "Content-Type: application/json" \
    -H "Authorization: Bearer $JWT_TOKEN" \
    --data '{"jsonrpc":"2.0","method":"eth_sendRawTransaction","params":["02f8718227d880849502f900849502f90083030d4094b42a8c62f3278afc9343a8fccd5232cbe8aa5117844190ab0080c080a098913733bc37a052351fadc62ec860dc341c9f1c6876801097b42514604c7657a05d8529fba214e8562803529af696cdca2f8d5545ca05f1bd2328ef9c175f57d9"],"id":1}' \
    http://localhost:8546 \
)
echo eth_sendRawTransaction RESPONSE $RESPONSE
TX1HASH=$(echo $RESPONSE | jq --raw-output '.result')
echo TX1HASH=$TX1HASH

# exit if the transaction is not sent
if [ "$TX1HASH" == "null" ]; then
  echo "Transaction not sent"
  exit 1
fi

# sending RLP encoded form of:
# transaction = {
#     'from': "0x38e3E7Aca6762E296F659Fcb4E460a3A621dcD3D",
#     'to': "0xc390cC49a32736a58733Cf46bE42f734dD4f53cb",
#     'value': 1000000000,
#     'nonce': 1,
#     'gas': 200000,
#     'maxFeePerGas': 2000000000,
#     'maxPriorityFeePerGas': 1000000000,
#     'chainId': 10200
# }
# signed using pvt key: 0x000038e28d32db8e509354d6b359eb58646e84bc942e3c79f470b08ebc976e1c

RESPONSE=$(curl -X POST -H "Content-Type: application/json" \
    -H "Authorization: Bearer $JWT_TOKEN" \
    --data '{"jsonrpc":"2.0","method":"eth_sendRawTransaction","params":["02f8718227d801843b9aca00847735940083030d4094c390cc49a32736a58733cf46be42f734dd4f53cb843b9aca0080c080a0ce361e8723595cb0824b3ed63cf2df8d93a4894cdfeb9dcc61ed6b413fc1cb4ca01625849ee2d550c2c280078590270be30745d454f2423df3beb3de1ba744e397"],"id":2}' \
    http://localhost:8546 \
)
echo eth_sendRawTransaction RESPONSE $RESPONSE
TX2HASH=$(echo $RESPONSE | jq --raw-output '.result')
echo TX2HASH=$TX2HASH

if [ "$TX2HASH" == "null" ]; then
  echo "Transaction not sent"
  exit 1
fi

# sleep for 1 sec
sleep 4

# Fetch producing block by payload ID

RESPONSE=$(curl -X POST -H "Content-Type: application/json" \
    -H "Authorization: Bearer $JWT_TOKEN" \
    --data "{
        \"jsonrpc\":\"2.0\",
        \"method\":\"engine_getPayloadV1\",
        \"params\":[
        \"$PAYLOAD_ID\"
        ],
        \"id\":1
    }" \
    http://localhost:8546 \
)
echo engine_getPayloadV1 RESPONSE $RESPONSE

BLOCK=$(echo $RESPONSE | jq '.result')
# BLOCK_NUMBER_HEX = 0x1, 0x2, etc
BLOCK_NUMBER_HEX_PREFIX=$(echo $BLOCK | jq --raw-output '.blockNumber')
BLOCK_NUMBER_HEX=${BLOCK_NUMBER_HEX_PREFIX#"0x"}
BLOCK_NUMBER=$((16#$BLOCK_NUMBER_HEX))
BLOCK_HASH=$(echo $BLOCK | jq --raw-output '.blockHash')

 # persist the block as test-vector
 
 echo $BLOCK | jq '.' > $OUT_DIR/block_$BLOCK_NUMBER_HEX.json

# send the new block as payload
  
RESPONSE=$(curl -X POST -H "Content-Type: application/json" \
    -H "Authorization: Bearer $JWT_TOKEN" \
    --data "{
      \"jsonrpc\":\"2.0\",
      \"method\":\"engine_newPayloadV1\",
      \"params\":[
        $BLOCK
      ],
      \"id\":1
    }" \
    http://localhost:8546 \
)
echo engine_newPayloadV1 with new block RESPONSE $RESPONSE


# set the block as head

RESPONSE=$(curl -X POST -H "Content-Type: application/json" \
    -H "Authorization: Bearer $JWT_TOKEN" \
    --data "{
      \"jsonrpc\":\"2.0\",
      \"method\":\"engine_forkchoiceUpdatedV1\",
      \"params\":[
        {
          \"headBlockHash\": \"$BLOCK_HASH\",
          \"safeBlockHash\": \"0x0000000000000000000000000000000000000000000000000000000000000000\",
          \"finalizedBlockHash\": \"0x0000000000000000000000000000000000000000000000000000000000000000\"
        },
        null
      ],
      \"id\":1
    }" \
    http://localhost:8546 \
)
echo engine_forkchoiceUpdatedV1 set new block as head RESPONSE $RESPONSE