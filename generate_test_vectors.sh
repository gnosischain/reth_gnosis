#!/bin/bash
## Exit immediately if any command exits with a non-zero status
set -e

# Clean up existing container if it exists
docker rm -f neth-vec-gen 2>/dev/null

# Start the container in the background
docker run --name neth-vec-gen --rm -d \
  -v $PWD/networkdata:/networkdata \
  -p 8545:8545 \
  -p 8546:8546 \
  nethermind/nethermind \
  --config=none \
  --Init.ChainSpecPath=/networkdata/chainspec.json \
  --Init.DiscoveryEnabled=false \
  --JsonRpc.Enabled=true \
  --JsonRpc.Host=0.0.0.0 \
  --JsonRpc.Port=8545 \
  --JsonRpc.EngineHost=0.0.0.0 \
  --JsonRpc.EnginePort=8546 \
  --JsonRpc.JwtSecretFile=/networkdata/jwtsecret \
  --TraceStore.Enabled=true 
  # --Init.ExitOnBlockNumber=4 \

# Capture the logs in the background
docker logs -f neth-vec-gen &

# Retry the curl command until it succeeds
until curl -X POST -H "Content-Type: application/json" \
  --data '{"jsonrpc":"2.0","method":"eth_getBlockByNumber","params":["0x0", false],"id":1}' \
  http://localhost:8545; do
    echo "Retrying..."
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
  BLOCK_NUMBER_HEX=$(echo $BLOCK | jq --raw-output '.blockNumber')
  BLOCK_HASH=$(echo $BLOCK | jq --raw-output '.blockHash')

  # persist the block as test-vector

  echo $BLOCK | jq '.' > block_$BLOCK_NUMBER_HEX.json

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
  make_block
done

# Clean up container
docker rm -f neth-vec-gen 2>/dev/null

