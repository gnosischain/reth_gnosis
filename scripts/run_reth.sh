#!/bin/bash

# reth flags from https://github.com/kurtosis-tech/ethereum-package/blob/7f365da6607bd863b12170ed475b77f4fafcc146/src/el/reth/reth_launcher.star#L206
#
# reth genesis from https://github.com/ethpandaops/ethereum-genesis-generator/blob/a4b6733ea9d47b2b2ec497f5212f0265b83fb601/apps/el-gen/genesis_geth.py#L34

# if TMPDIR is empty, use /tmp
TMPDIR=${TMPDIR:-/tmp}

DATA_DIR=$TMPDIR/reth_test
# Ensure no data from previous tests
rm -rf $DATA_DIR

# Script's directory
DIR="$(dirname "$0")"

# Use the provided argument as the chain file or default to `chiado_genesis_alloc.json`
CHAIN_FILE=${1:-"$DIR/chiado_genesis_alloc.json"}

echo "Using chain file: $CHAIN_FILE"

# $PWD/target/release/reth \
cargo run -- \
  node \
  -vvvv \
  --chain=$CHAIN_FILE \
  --datadir=$DATA_DIR \
  --http \
  --http.port=8545 \
  --http.addr=0.0.0.0 \
  --http.corsdomain='*' \
  --http.api=admin,net,eth,web3,debug,trace \
  --authrpc.port=8546 \
  --authrpc.jwtsecret=$DIR/networkdata/jwtsecret \
  --authrpc.addr=0.0.0.0 \
  --port=0 \
  --disable-discovery

