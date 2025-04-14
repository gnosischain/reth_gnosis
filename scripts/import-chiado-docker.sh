#!/bin/bash
set -euo pipefail

DATA_DIR=$1
STATE_FILE="$DATA_DIR/state_at_700000.jsonl"
HEADER_FILE="$DATA_DIR/header_700000.rlp"

EXPECTED_STATE_ROOT="0x90b1762d6b81ea05b51aea094a071f7ec4c0742e2bb2d5d560d1833443ff40fd"

echo -e "\n\033[0;34mTrying to import state using Docker...\033[0m"

# if imported, exit
if [ -f "$DATA_DIR/imported" ]; then
    echo -e "\033[0;32mAlready imported!\033[0m"
    exit 0
fi

DB_PATH="$DATA_DIR/db"

# if it exists, check state root
if [ -d "$DB_PATH" ]; then
    echo -e "\033[0;34mChecking state root...\033[0m"
    
    STATE_ROOT=$(docker run --rm -v "$DATA_DIR":/data ghcr.io/gnosischain/reth_gnosis:master \
        --chain chainspecs/chiado.json \
        db --datadir /data get static-file headers 700000 | \
        grep stateRoot | sed -E 's/.*: "(0x[0-9a-f]+)".*/\1/') || {
        STATE_ROOT=""
    }

    echo -e "\033[0;34mState root: $STATE_ROOT\033[0m"

    if [ "$STATE_ROOT" != "$EXPECTED_STATE_ROOT" ]; then
        echo -e "\033[0;31mState root mismatch! Expected $EXPECTED_STATE_ROOT, got $STATE_ROOT\033[0m"
        echo -e "\033[0;31mClearing database...\033[0m"
        docker run --rm -v "$DATA_DIR":/data ghcr.io/gnosischain/reth_gnosis:master \
            --chain chainspecs/chiado.json \
            db --datadir /data drop -f || true
        echo -e "\033[0;34mDeleted existing DB due to corruption...\033[0m"
    else
        echo -e "\033[0;32mAlready imported. State root matches!\033[0m"
        touch "$DATA_DIR/imported"
        exit 0
    fi
fi

echo -e "\033[0;34mImporting the state...\033[0m"
docker run --rm -v "$DATA_DIR":/data ghcr.io/gnosischain/reth_gnosis:master \
    --chain chainspecs/chiado.json \
    init-state /data/state_at_700000.jsonl \
    --without-evm \
    --header /data/header_700000.rlp \
    --total-difficulty 231708131825107706987652208063906496124457284 \
    --header-hash cdc424294195555e53949b6043339a49b049b48caa8d85bc7d5f5d12a85964b6 \
    --datadir /data

STATE_ROOT=$(docker run --rm -v "$DATA_DIR":/data ghcr.io/gnosischain/reth_gnosis:master \
    --chain chainspecs/chiado.json \
    db --datadir /data get static-file headers 700000 | \
    grep stateRoot | sed -E 's/.*: "(0x[0-9a-f]+)".*/\1/')

if [ "$STATE_ROOT" != "$EXPECTED_STATE_ROOT" ]; then
    echo -e "\033[0;31mState root mismatch after import! Expected $EXPECTED_STATE_ROOT, got $STATE_ROOT\033[0m"
    exit 1
fi

touch "$DATA_DIR/imported"
echo -e "\033[0;32mState imported successfully using Docker!\033[0m"
