#!/bin/bash
set -e

# Script to run Nethermind dockerized and attach to it. 
# The jwtsecret is hardcoded, do not modify it.

# Script's directory
DIR="$(dirname "$0")"

# Clean up existing container if it exists
docker rm -f neth-vec-gen 2>/dev/null

# Start the container 
docker run --name neth-vec-gen --rm \
  -v $DIR/networkdata:/networkdata \
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

