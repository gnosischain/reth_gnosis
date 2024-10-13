#!/bin/bash
set -e

# Expects reth to be running on the background

# Script's directory
DIR="$(dirname "$0")"

OUT_DIR=$DIR/blocks

N=5

# Retry the curl command until it succeeds
until curl -X POST -H "Content-Type: application/json" \
  --data '{"jsonrpc":"2.0","method":"eth_getBlockByNumber","params":["0x0", false],"id":1}' \
  http://localhost:8545; do
    echo "Retrying..."
    sleep 2
done


function apply_block_file() {
  BLOCK_FILEPATH=$1
  BLOCK=$(<$BLOCK_FILEPATH)
  echo Applying $BLOCK

  # The ASCII representation of `2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a`
  JWT_SECRET="********************************"
  # Generate a JWT token using the secret key
  # jwt is this CLI tool https://github.com/mike-engel/jwt-cli/tree/main
  # iat is appended automatically
  JWT_TOKEN=$(jwt encode --alg HS256 --secret "$JWT_SECRET")
  echo JWT_TOKEN: $JWT_TOKEN

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

  BLOCK_HASH=$(echo $BLOCK | jq --raw-output '.blockHash')

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

  PAYLOAD_STATUS=$(echo $RESPONSE | jq --raw-output '.result.payloadStatus.status')
  echo PAYLOAD_STATUS: $PAYLOAD_STATUS
  # If the status is not "VALID", exit the script with a non-zero code to make CI fail
  if [ "$PAYLOAD_STATUS" != "VALID" ]; then
    echo "Error: Payload status is $PAYLOAD_STATUS, failing CI."
    exit 1
  fi
}


for ((i = 1; i <= N; i++)); do
  BLOCK_FILEPATH=$OUT_DIR/block_$i.json
  apply_block_file $BLOCK_FILEPATH
done

