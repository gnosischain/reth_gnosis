#!/bin/bash
set -e

# Script's directory
DIR="$(dirname "$0")"

sleep 3

"$DIR/run_reth.sh" &
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
    --data '{"jsonrpc":"2.0","method":"eth_sendRawTransaction","params":["02f8708227d801849502f900849502f90083030d4094b42a8c62f3278afc9343a8fccd5232cbe8aa5117844190ab0080c0809fe483006f558948cb15b00a3f17c706f2c6ae084c131fca3a84042b23be3f51a05c550f5d70d8c6deb405250cb75e68d5c7daee4e7c202d841df0338b9fcd0838"],"id":2}' \
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