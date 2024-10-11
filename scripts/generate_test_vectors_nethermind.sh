#!/bin/bash
set -e

# Script to generate test vectors from Nethermind. It connects to the engine API of Nethermid to produce
# blocks on the genesis block and stores them in $OUT_DIR. The jwtsecret is hardcoded, do not modify it.
# To run just do:
#
# ```
# ./generate_test_vectors_nethermind.sh
# ```

# Script's directory
DIR="$(dirname "$0")"

$DIR/run_nethermind.sh &
BG_PID=$!

# Set the trap to call cleanup
cleanup() {
  echo "Stopping node process (PID: $BG_PID)..."
  kill $BG_PID 2>/dev/null || true
  # Also force clean the docker container, killing the attached process is not enough
  docker rm -f neth-vec-gen 2>/dev/null || true
}
trap cleanup EXIT

# Function to check if Nethermind is available
check_nethermind_availability() {
  until curl -X POST -H "Content-Type: application/json" \
    --data '{"jsonrpc":"2.0","method":"eth_getBlockByNumber","params":["0x0", false],"id":1}' \
    http://localhost:8545; do
    echo "Retrying..."
    sleep 2
  done
  echo "Nethermind is available"
  return 0
}

# Wait for Nethermind to become available
while ! check_nethermind_availability; do
  sleep 2
done

# Generate test vectors
$DIR/generate_test_vectors.sh