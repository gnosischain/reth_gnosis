#!/bin/bash
set -euo pipefail

DATA_DIR=$1
STATE_FILE=$DATA_DIR/state_2648650.jsonl
HEADER_FILE=$DATA_DIR/header_26478650.rlp

EXPECTED_STATE_ROOT="0x95c4ecc49287d652e956b71ef82fb34a17da87fcbd6ab64f05542ddd3b5cb44f"

echo -e "\n\033[0;34mTrying to import state using Docker...\033[0m"

DB_PATH="$DATA_DIR/db"

# if it exists, check state root
if [ -d "$DB_PATH" ]; then
    echo -e "\033[0;34mChecking state root...\033[0m"
    
    STATE_ROOT=$(docker run --rm -v "$DATA_DIR":/data reth \
        --chain chainspecs/gnosis.json \
        db --datadir /data get static-file headers 26478650 | \
        grep stateRoot | sed -E 's/.*: "(0x[0-9a-f]+)".*/\1/') || {
        STATE_ROOT=""
    }

    echo -e "\033[0;34mState root: $STATE_ROOT\033[0m"

    if [ "$STATE_ROOT" != "$EXPECTED_STATE_ROOT" ]; then
        echo -e "\033[0;31mState root mismatch! Expected $EXPECTED_STATE_ROOT, got $STATE_ROOT\033[0m"
        echo -e "\033[0;31mClearing database...\033[0m"
        docker run --rm -v "$DATA_DIR":/data reth \
            --chain chainspecs/gnosis.json \
            db --datadir /data drop -f || true
        echo -e "\033[0;34mDeleted existing DB due to corruption...\033[0m"
    else
        echo -e "\033[0;32mAlready imported. State root matches!\033[0m"
        touch "$DATA_DIR/imported"
        exit 0
    fi
fi

echo -e "\033[0;34mImporting the state...\033[0m"
docker run --rm -v "$DATA_DIR":/data reth \
    --chain chainspecs/gnosis.json \
    init-state /data/state_26478650.jsonl \
    --without-evm \
    --header /data/header_26478650.rlp \
    --total-difficulty 8626000110427540000000000000000000000000000000 \
    --header-hash a133198478cb01b4585604d07f584633f1f147103b49672d2bd87a5a3ba2c06e \
    --datadir /data

STATE_ROOT=$(docker run --rm -v "$DATA_DIR":/data reth \
    --chain chainspecs/gnosis.json \
    db --datadir /data get static-file headers 26478650 | \
    grep stateRoot | sed -E 's/.*: "(0x[0-9a-f]+)".*/\1/')

if [ "$STATE_ROOT" != "$EXPECTED_STATE_ROOT" ]; then
    echo -e "\033[0;31mState root mismatch after import! Expected $EXPECTED_STATE_ROOT, got $STATE_ROOT\033[0m"
    exit 1
fi

touch "$DATA_DIR/imported"
echo -e "\033[0;32mState imported successfully using Docker!\033[0m"
