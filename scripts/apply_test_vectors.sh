#!/bin/bash
set -e

# Expects reth to be running on the background

# Script's directory
DIR="$(dirname "$0")"

BLOCKS_FOLDER=${1:-blocks}
OUT_DIR=$DIR/$BLOCKS_FOLDER

N=${2:-5}

# Retry the curl command until it succeeds
until curl -X POST -H "Content-Type: application/json" \
  --data '{"jsonrpc":"2.0","method":"eth_getBlockByNumber","params":["0x0", false],"id":1}' \
  http://localhost:8545; do
    echo "Retrying..."
    sleep 2
done


CANCUN_START_TIME=1800000000
PECTRA_START_TIME=1900000000


function apply_block_file() {
  BLOCK_FILEPATH=$1
  BLOCK=$(<$BLOCK_FILEPATH)
  echo Applying $BLOCK

  BLOCK_TIME_HEX_PREFIX=$(echo $BLOCK | jq --raw-output '.timestamp')
  BLOCK_TIME_HEX=${BLOCK_TIME_HEX_PREFIX#"0x"}
  BLOCK_TIME=$((16#$BLOCK_TIME_HEX))

  # The ASCII representation of `2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a`
  JWT_SECRET="********************************"
  # Generate a JWT token using the secret key
  # jwt is this CLI tool https://github.com/mike-engel/jwt-cli/tree/main
  # iat is appended automatically
  JWT_TOKEN=$(jwt encode --alg HS256 --secret "$JWT_SECRET")
  echo JWT_TOKEN: $JWT_TOKEN

  BLOCK_HASH=$(echo $BLOCK | jq --raw-output '.blockHash')

  # if block time is less than CANCUN_START_TIME
  if [ $BLOCK_TIME -lt $CANCUN_START_TIME ]; then
    echo "Block time is less than CANCUN_START_TIME"
    RESPONSE=$(curl -X POST -H "Content-Type: application/json" \
      -H "Authorization: Bearer $JWT_TOKEN" \
      --data "{
        \"jsonrpc\":\"2.0\",
        \"method\":\"engine_newPayloadV2\",
        \"params\":[
          $BLOCK
        ],
        \"id\":1
      }" \
      http://localhost:8546 \
    )
    echo engine_newPayloadV2 with new block RESPONSE $RESPONSE

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
  fi

  # if block time is less than PECTRA_START_TIME but greater than or equal to CANCUN_START_TIME
  if [ $BLOCK_TIME -ge $CANCUN_START_TIME ] && [ $BLOCK_TIME -lt $PECTRA_START_TIME ]; then
    echo "Block time is less than PECTRA_START_TIME but greater than or equal to CANCUN_START_TIME"
    VERSIONEDHASH_FILEPATH=$2
    VERSIONEDHASH=$(<$VERSIONEDHASH_FILEPATH)

    RESPONSE=$(curl -X POST -H "Content-Type: application/json" \
      -H "Authorization: Bearer $JWT_TOKEN" \
      --data "{
        \"jsonrpc\":\"2.0\",
        \"method\":\"engine_newPayloadV3\",
        \"params\":[
          $BLOCK,
          $VERSIONEDHASH,
          \"0x1100000000000000000000000000000000000000000000000000000000000000\"
        ],
        \"id\":1
      }" \
      http://localhost:8546 \
    )

    RESPONSE=$(curl -X POST -H "Content-Type: application/json" \
      -H "Authorization: Bearer $JWT_TOKEN" \
      --data "{
        \"jsonrpc\":\"2.0\",
        \"method\":\"engine_forkchoiceUpdatedV3\",
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
    echo engine_forkchoiceUpdatedV3 set new block as head RESPONSE $RESPONSE

    PAYLOAD_STATUS=$(echo $RESPONSE | jq --raw-output '.result.payloadStatus.status')
    echo PAYLOAD_STATUS: $PAYLOAD_STATUS
    # If the status is not "VALID", exit the script with a non-zero code to make CI fail
    if [ "$PAYLOAD_STATUS" != "VALID" ]; then
      echo "Error: Payload status is $PAYLOAD_STATUS, failing CI."
      exit 1
    fi
  fi

  # if block time is greater than or equal to PECTRA_START_TIME
  if [ $BLOCK_TIME -ge $PECTRA_START_TIME ]; then
    echo "Block time is greater than or equal to PECTRA_START_TIME"
    RESPONSE=$(curl -X POST -H "Content-Type: application/json" \
        -H "Authorization: Bearer $JWT_TOKEN" \
        --data "{
          \"jsonrpc\":\"2.0\",
          \"method\":\"engine_newPayloadV4\",
          \"params\":[
            $BLOCK,
            [\"0x01af254f4973a787397a71597d9492c0d7e52c3d80b42dd51f7ae249954c57bd\"],
            \"0x1100000000000000000000000000000000000000000000000000000000000000\",
            []
          ],
          \"id\":1
        }" \
        http://localhost:8546 \
    )
    echo engine_newPayloadV4 with new block RESPONSE $RESPONSE

    # set the block as head

    RESPONSE=$(curl -X POST -H "Content-Type: application/json" \
        -H "Authorization: Bearer $JWT_TOKEN" \
        --data "{
          \"jsonrpc\":\"2.0\",
          \"method\":\"engine_forkchoiceUpdatedV3\",
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
    echo engine_forkchoiceUpdatedV3 set new block as head RESPONSE $RESPONSE

    PAYLOAD_STATUS=$(echo $RESPONSE | jq --raw-output '.result.payloadStatus.status')
    echo PAYLOAD_STATUS: $PAYLOAD_STATUS
    # If the status is not "VALID", exit the script with a non-zero code to make CI fail
    if [ "$PAYLOAD_STATUS" != "VALID" ]; then
      echo "Error: Payload status is $PAYLOAD_STATUS, failing CI."
      exit 1
    fi
  fi
}


for ((i = 1; i <= N; i++)); do
  BLOCK_FILEPATH=$OUT_DIR/block_$i.json
  VERSIONEDHASH_FILEPATH=$OUT_DIR/versioned_hash_$i.json
  apply_block_file $BLOCK_FILEPATH $VERSIONEDHASH_FILEPATH
done
