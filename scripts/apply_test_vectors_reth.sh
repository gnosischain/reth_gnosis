#!/bin/bash
set -e

# Script's directory
DIR="$(dirname "$0")"

$DIR/run_reth.sh &
BG_PID=$!

BLOCKS_FOLDER=${1:-blocks}
N=${2:-5}

# Set the trap to call cleanup if an error occurs
cleanup() {
  echo "Stopping node process (PID: $BG_PID)..."
  kill $BG_PID 2>/dev/null || true

  pkill -f "reth node" || true
}
trap cleanup EXIT

$DIR/apply_test_vectors.sh $BLOCKS_FOLDER $N


