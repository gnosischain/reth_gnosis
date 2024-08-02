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
  --Init.ExitOnBlockNumber=1 \
  --JsonRpc.Enabled=true \
  --JsonRpc.Host=0.0.0.0 \
  --JsonRpc.Port=8545 \
  --JsonRpc.EngineHost=0.0.0.0 \
  --JsonRpc.EnginePort=8546 \
  --JsonRpc.JwtSecretFile=/networkdata/jwtsecret \
  --TraceStore.Enabled=true 

# Capture the logs in the background
docker logs -f neth-vec-gen &

# Retry the curl command until it succeeds
until curl -X POST -H "Content-Type: application/json" \
  --data '{"jsonrpc":"2.0","method":"eth_getBlockByNumber","params":["0x0", false],"id":1}' \
  http://localhost:8545; do
    echo "Retrying..."
    sleep 2
done

GENESIS_BLOCK=$(curl -X POST -H "Content-Type: application/json" \
  --data '{"jsonrpc":"2.0","method":"eth_getBlockByNumber","params":["0x0", false],"id":1}' \
  http://localhost:8545)

# --raw-output remove the double quotes
GENESIS_HASH=$(echo $GENESIS_BLOCK | jq --raw-output '.result.hash')
echo GENESIS_HASH=$GENESIS_HASH

# The ASCII representation of `2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a`
JWT_SECRET="********************************"

# Generate a JWT token using the secret key
# jwt is this CLI tool https://github.com/mike-engel/jwt-cli/tree/main
# iat is appended automatically
JWT_TOKEN=$(jwt encode --alg HS256 --secret "$JWT_SECRET")

echo JWT_TOKEN: $JWT_TOKEN

# Request to produce block on current head

RESPONSE=$(curl -X POST -H "Content-Type: application/json" \
  -H "Authorization: Bearer $JWT_TOKEN" \
  --data "{
    \"jsonrpc\":\"2.0\",
    \"method\":\"engine_forkchoiceUpdatedV1\",
    \"params\":[
      {
        \"headBlockHash\": \"$GENESIS_HASH\",
        \"safeBlockHash\": \"0x0000000000000000000000000000000000000000000000000000000000000000\",
        \"finalizedBlockHash\": \"0x0000000000000000000000000000000000000000000000000000000000000000\"
      },
      {
        \"timestamp\": 1700000000,
        \"prevRandao\": \"0x0000000000000000000000000000000000000000000000000000000000000000\",
        \"suggestedFeeRecipient\": \"0x0000000000000000000000000000000000000000\"
      }
    ],
    \"id\":1
  }" \
  http://localhost:8546 \
)
echo engine_forkchoiceUpdatedV1 RESPONSE $RESPONSE

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

# Clean up container
docker rm -f neth-vec-gen 2>/dev/null

