#!/bin/bash
set -e

RESPONSE=$(curl -X POST -H "Content-Type: application/json" \
  --data '{
    "jsonrpc":"2.0",
    "method":"trace_block",
    "params":["0x1"],
    "id":1
  }' \
  http://localhost:8545)
echo trace_block RESPONSE $RESPONSE

RESPONSE=$(curl -X POST -H "Content-Type: application/json" \
  --data '{
    "jsonrpc":"2.0",
    "method":"eth_getAccount",
    "params":["0x2000000000000000000000000000000000000001","0x1"],
    "id":1
  }' \
  http://localhost:8545)
echo eth_getAccount RESPONSE $RESPONSE

