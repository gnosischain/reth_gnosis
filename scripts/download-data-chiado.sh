#!/bin/bash
set -e

# first input or $PWD/data
DATA_DIR=$1

STATE_FILE=$DATA_DIR/state_at_700000.jsonl
HEADER_FILE=$DATA_DIR/header_700000.rlp

echo -e "State directory: \033[0;32m$DATA_DIR\033[0m"

# If either file is missing, delete the data directory and download them
if [ ! -f $STATE_FILE ] || [ ! -f $HEADER_FILE ]; then
    echo "Either $STATE_FILE or $HEADER_FILE is missing. Deleting the data directory and downloading the files."
    rm -rf $DATA_DIR
    mkdir -p $DATA_DIR
    wget https://media.githubusercontent.com/media/gnosischain/reth-init-state/refs/heads/main/chiado/state_700000.jsonl -O "$STATE_FILE.temp"
    mv "$STATE_FILE.temp" "$STATE_FILE"
    wget https://media.githubusercontent.com/media/gnosischain/reth-init-state/refs/heads/main/chiado/header_700000.rlp -O "$HEADER_FILE.temp"
    mv "$HEADER_FILE.temp" "$HEADER_FILE"
    echo "Files downloaded successfully."
fi
