#!/bin/bash
set -e

# Script's directory
DIR="$(dirname "$0")"

sleep 3

"$DIR/run_reth.sh" $DIR/genesis_alloc_eip1559.json &
BG_PID=$!

# Set the trap to call cleanup if an error occurs
cleanup() {
  echo "Stopping node process (PID: $BG_PID)..."
  ps aux | grep "reth node" | grep -v grep | awk '{print $2}' | xargs kill
  kill $BG_PID 2>/dev/null || true
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

RESPONSE=$(curl -X POST -H "Content-Type: application/json" \
    -H "Authorization: Bearer $JWT_TOKEN" \
    --data '{"jsonrpc":"2.0","method":"eth_sendRawTransaction","params":["0x02f86e8227d8808456b989c08456b989cb825208940ccdd4caf542282a020ea455abe0edfe968763228203e880c080a02f932486d36949a6f15a08d019f8a276d2717eeef872df2b5a1d0f2dc425dd3ca079cb1247e7510c1fd8191089a66d581b0eaae586e3c1060ac02fb2793f20e69e"],"id":1}' \
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

RESPONSE=$()

RESPONSE=$(curl -X POST -H "Content-Type: application/json" \
    -H "Authorization: Bearer $JWT_TOKEN" \
    --data '{"jsonrpc":"2.0","method":"eth_sendRawTransaction","params":["0x02f8718227d880843b9aca00847735940083030d4094c390cc49a32736a58733cf46be42f734dd4f53cb843b9aca0080c080a0a4f4d99483deb86d07f82d3d0a993eaef52bfb64aeeaf7bd847b1e2131a2d265a05443ba597754edad76f5eebec21863289bde25f665aa7e912f904e1f2b91c89b"],"id":2}' \
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
sleep 1

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
echo HEAD_BLOCK $HEAD_BLOCK

BASE_FEE_PER_GAS_HEX_PREFIX=$(echo $HEAD_BLOCK | jq --raw-output '.result.baseFeePerGas')
BASE_FEE_PER_GAS_HEX=${BASE_FEE_PER_GAS_HEX_PREFIX#"0x"}
BASE_FEE_PER_GAS=$((16#$BASE_FEE_PER_GAS_HEX))

GAS_USED_HEX_PREFIX=$(echo $HEAD_BLOCK | jq --raw-output '.result.gasUsed')
GAS_USED_HEX=${GAS_USED_HEX_PREFIX#"0x"}
GAS_USED=$((16#$GAS_USED_HEX))

echo DECIMAL BASE_FEE_PER_GAS $BASE_FEE_PER_GAS GAS_USED $GAS_USED

TX1RECEIPT=$(curl http://localhost:8545 \
    -X POST \
    -H "Content-Type: application/json" \
    -H "Authorization: Bearer $JWT_TOKEN" \
    --data '{"method":"eth_getTransactionReceipt","params":["'$TX1HASH'"],"id":1,"jsonrpc":"2.0"}'
)
echo eth_getTransactionReceipt "0x1" RESPONSE $TX1RECEIPT

TX1_EFF_GAS_PRICE_HEX_PREFIX=$(echo $TX1RECEIPT | jq --raw-output '.result.effectiveGasPrice')
TX1_EFF_GAS_PRICE_HEX=${TX1_EFF_GAS_PRICE_HEX_PREFIX#"0x"}
TX1_EFF_GAS_PRICE=$((16#$TX1_EFF_GAS_PRICE_HEX))

TX1_GAS_USED_HEX_PREFIX=$(echo $TX1RECEIPT | jq --raw-output '.result.gasUsed')
TX1_GAS_USED_HEX=${TX1_GAS_USED_HEX_PREFIX#"0x"}
TX1_GAS_USED=$((16#$TX1_GAS_USED_HEX))
  
TX2RECEIPT=$(curl http://localhost:8545 \
    -X POST \
    -H "Content-Type: application/json" \
    -H "Authorization: Bearer $JWT_TOKEN" \
    --data '{"method":"eth_getTransactionReceipt","params":["'$TX2HASH'"],"id":1,"jsonrpc":"2.0"}'
)
echo eth_getTransactionReceipt "0x1" RESPONSE $TX2RECEIPT

TX2_EFF_GAS_PRICE_HEX_PREFIX=$(echo $TX2RECEIPT | jq --raw-output '.result.effectiveGasPrice')
TX2_EFF_GAS_PRICE_HEX=${TX2_EFF_GAS_PRICE_HEX_PREFIX#"0x"}
TX2_EFF_GAS_PRICE=$((16#$TX2_EFF_GAS_PRICE_HEX))

TX2_GAS_USED_HEX_PREFIX=$(echo $TX2RECEIPT | jq --raw-output '.result.gasUsed')
TX2_GAS_USED_HEX=${TX2_GAS_USED_HEX_PREFIX#"0x"}
TX2_GAS_USED=$((16#$TX2_GAS_USED_HEX))

echo TX1_EFF_GAS_PRICE $TX1_EFF_GAS_PRICE TX2_EFF_GAS_PRICE $TX2_EFF_GAS_PRICE

RESPONSE=$(curl -X POST -H "Content-Type: application/json" --data '{"jsonrpc":"2.0","method":"eth_getBalance","params":["0x1559000000000000000000000000000000000000", "latest"],"id":1}' http://localhost:8545)
echo collector balance RESPONSE $RESPONSE

COLLECTOR_BALANCE_HEX_PREFIX=$(echo $RESPONSE | jq --raw-output '.result')
COLLECTOR_BALANCE_HEX=${COLLECTOR_BALANCE_HEX_PREFIX#"0x"}
COLLECTOR_BALANCE=$((16#$COLLECTOR_BALANCE_HEX))

RESPONSE=$(curl -X POST -H "Content-Type: application/json" --data '{"jsonrpc":"2.0","method":"eth_getBalance","params":["0x0000000000000000000000000000000000000000", "latest"],"id":1}' http://localhost:8545)
echo fee_recipient balance RESPONSE $RESPONSE

FEE_RECIPIENT_BALANCE_HEX_PREFIX=$(echo $RESPONSE | jq --raw-output '.result')
FEE_RECIPIENT_BALANCE_HEX=${FEE_RECIPIENT_BALANCE_HEX_PREFIX#"0x"}
FEE_RECIPIENT_BALANCE=$((16#$FEE_RECIPIENT_BALANCE_HEX))

TIP_1_FEE=$((TX1_EFF_GAS_PRICE - BASE_FEE_PER_GAS))
TIP_1=$((TIP_1_FEE * TX1_GAS_USED))

TIP_2_FEE=$((TX2_EFF_GAS_PRICE - BASE_FEE_PER_GAS))
TIP_2=$((TIP_2_FEE * TX2_GAS_USED))

TOTAL_TIP=$((TIP_1 + TIP_2))
echo TOTAL_TIP $TOTAL_TIP

TOTAL_BASE_FEE=$((BASE_FEE_PER_GAS * GAS_USED))
echo TOTAL_BASE_FEE $TOTAL_BASE_FEE

if ((COLLECTOR_BALANCE != TOTAL_BASE_FEE)); then
  echo "Collector balance is not equal to total base fee"
  exit 1
fi

if ((FEE_RECIPIENT_BALANCE != TOTAL_TIP)); then
  echo "Fee recipient balance is not equal to total tip"
  exit 1
fi

echo "Collector balance is equal to total base fee and fee recipient balance is equal to total tip"