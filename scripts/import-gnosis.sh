#!/bin/bash
set -euo pipefail

DATA_DIR=$1
STATE_FILE=$DATA_DIR/state_26478650.jsonl
HEADER_FILE=$DATA_DIR/header_26478650.rlp

SCRIPT_DIR="$(dirname "$(realpath "$0")")"

echo -e "\n\033[0;34mTrying to import state...\033[0m"

# if imported, exit
if [ -f "$DATA_DIR/imported" ]; then
    echo -e "\033[0;32mAlready imported!\033[0m"
    exit 0
fi

EXPECTED_STATE_ROOT="0x95c4ecc49287d652e956b71ef82fb34a17da87fcbd6ab64f05542ddd3b5cb44f"

DB_PATH="$DATA_DIR/db"
# if it exists, check state root
if [ -d "$DB_PATH" ]; then
    echo -e "\033[0;34mChecking state root...\033[0m"
    
    STATE_ROOT=$(./target/debug/reth --chain "$SCRIPT_DIR/chainspecs/gnosis.json" db --datadir "$DATA_DIR" get static-file headers 26478650 | grep stateRoot | sed -E 's/.*: "(0x[0-9a-f]+)".*/\1/') || {
        STATE_ROOT=""
    }
    echo -e "\033[0;34mState root: $STATE_ROOT\033[0m"
    
    if [ "$STATE_ROOT" != "$EXPECTED_STATE_ROOT" ]; then
        echo -e "\033[0;31mState root mismatch! Expected $EXPECTED_STATE_ROOT, got $STATE_ROOT\033[0m"
        echo -e "\033[0;31mClearing database...\033[0m"
        ./target/debug/reth --chain $SCRIPT_DIR/chainspecs/gnosis.json db --datadir "$DATA_DIR" drop -f || true
        echo -e "\033[0;34mDeleted existing DB due to corruption...\033[0m"
    else
        echo -e "\033[0;32mAlready imported. State root matches!\033[0m"
        touch $DATA_DIR/imported
        exit 0
    fi
fi

echo -e "\033[0;34mImporting the state...\033[0m"
./target/debug/reth --chain "$SCRIPT_DIR/chainspecs/gnosis.json" init-state $STATE_FILE --without-evm --header $HEADER_FILE --total-difficulty 8626000110427540000000000000000000000000000000 --header-hash a133198478cb01b4585604d07f584633f1f147103b49672d2bd87a5a3ba2c06e --datadir $DATA_DIR

STATE_ROOT=$(./target/debug/reth --chain "$SCRIPT_DIR/chainspecs/gnosis.json" db --datadir "$DATA_DIR" get static-file headers 26478650 | grep stateRoot | sed -E 's/.*: "(0x[0-9a-f]+)".*/\1/')
if [ "$STATE_ROOT" != "$EXPECTED_STATE_ROOT" ]; then
    echo -e "\033[0;31mState root mismatch! Expected $EXPECTED_STATE_ROOT, got $STATE_ROOT\033[0m"
    exit 1
fi
touch $DATA_DIR/imported
echo -e "\033[0;32mState imported successfully!\033[0m"
