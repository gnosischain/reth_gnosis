#!/bin/bash

docker run --name neth-vec-gen --rm \
  -v $PWD/networkdata:/networkdata \
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
