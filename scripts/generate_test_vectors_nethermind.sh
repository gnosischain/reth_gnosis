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

$DIR/generate_test_vectors.sh
