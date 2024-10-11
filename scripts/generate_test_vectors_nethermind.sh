#!/bin/bash
set -e

# Script's directory
DIR="$(dirname "$0")"

# Start Nethermind and capture its PID
$DIR/run_nethermind.sh &
BG_PID=$!

# Function to stop Nethermind and cleanup Docker
cleanup() {
  echo "Stopping node process (PID: $BG_PID)..."
  kill $BG_PID 2>/dev/null || true
  # Clean up the docker container
  docker rm -f neth-vec-gen 2>/dev/null || true
}
trap cleanup EXIT

# Wait for Nethermind to be ready
echo "Waiting for Nethermind to become ready..."
RETRY_COUNT=0
MAX_RETRIES=10
NETHEMIND_READY=false

while [ $RETRY_COUNT -lt $MAX_RETRIES ]; do
  if curl -s http://localhost:8545 >/dev/null; then
    echo "Nethermind is ready!"
    NETHEMIND_READY=true
    break
  fi
  echo "Nethermind is not ready yet. Retrying... ($((RETRY_COUNT+1))/$MAX_RETRIES)"
  sleep 5
  RETRY_COUNT=$((RETRY_COUNT+1))
done

if [ "$NETHEMIND_READY" = false ]; then
  echo "Nethermind failed to become ready after $MAX_RETRIES retries. Exiting."
  exit 1
fi

# Generate test vectors
$DIR/generate_test_vectors.sh

