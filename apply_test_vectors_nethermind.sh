#!/bin/bash
set -e

./run_nethermind.sh &
./apply_test_vectors.sh
# TODO more resilient shutdown
docker rm -f neth-vec-gen 2>/dev/null

