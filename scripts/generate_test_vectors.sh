#!/bin/bash
set -e

# Script to generate test vectors for a running client. It connects to the engine API at :8546 to produce
# blocks on the genesis block and stores them in $OUT_DIR. The jwtsecret is hardcoded, do not modify it.
# To run just do:
#
# ```
# ./generate_test_vectors.sh
# ```

# Script's directory
DIR="$(dirname "$0")"

OUT_DIR=$DIR/blocks
mkdir -p $OUT_DIR


# Retry the curl command until it succeeds
# Function to check if Nethermind is available
check_nethermind_availability() {
  until curl -X POST -H "Content-Type: application/json" \
    --data '{"jsonrpc":"2.0","method":"eth_getBlockByNumber","params":["0x0", false],"id":1}' \
    http://localhost:8545; do
    echo "Retrying..."
    sleep 2
  done
  echo "Nethermind is available"
  # return 0
}

# Wait for Nethermind to become available
while ! check_nethermind_availability; do
  sleep 2
done

BLOCK_COUNTER=0

function make_block() {
  ((BLOCK_COUNTER++))

  HEAD_BLOCK=$(curl -X POST -H "Content-Type: application/json" \
    --data '{"jsonrpc":"2.0","method":"eth_getBlockByNumber","params":["latest", false],"id":1}' \
    http://localhost:8545)

  # --raw-output remove the double quotes
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

}

# Number of times to call make_block
N=5

for ((i = 1; i <= N; i++)); do
  echo "Making block $i"
  make_block
done

