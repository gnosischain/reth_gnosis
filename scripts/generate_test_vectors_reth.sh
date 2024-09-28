#!/bin/bash
set -e

# Script to generate test vectors from Reth. It connects to the engine API of Reth to produce
# blocks on the genesis block and stores them in $OUT_DIR. The jwtsecret is hardcoded, do not modify it.
# To run just do:
#
# ```
# ./generate_test_vectors_reth.sh
# ```

# Script's directory
DIR="$(dirname "$0")"

$DIR/run_reth.sh &
BG_PID=$!

# Set the trap to call cleanup if an error occurs
cleanup() {
  echo "Stopping node process (PID: $BG_PID)..."
  kill $BG_PID 2>/dev/null || true
}
trap cleanup EXIT

$DIR/generate_test_vectors.sh

