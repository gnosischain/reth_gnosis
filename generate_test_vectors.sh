#!/bin/bash

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


# Clean up container
docker rm -f neth-vec-gen 2>/dev/null

