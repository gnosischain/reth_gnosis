#!/bin/bash
set -euo pipefail

# Cleanup function
cleanup() {
    echo "Interrupted. Cleaning up temp files..."
    rm -f "$STATE_FILE.temp" "$HEADER_FILE.temp"
    exit 1
}
trap cleanup INT TERM

# first input or $PWD/data
DATA_DIR=$1

STATE_FILE=$DATA_DIR/state_at_700000.jsonl
HEADER_FILE=$DATA_DIR/header_700000.rlp

echo -e "\033[0;34mTrying to download files...\033[0m"

# Download state file if missing
if [ ! -f "$STATE_FILE" ]; then
    echo -e "\033[0;33mMissing $STATE_FILE. Downloading...\033[0m"
    wget -O "$STATE_FILE.temp" https://media.githubusercontent.com/media/gnosischain/reth-init-state/refs/heads/main/chiado/state_700000.jsonl
    mv "$STATE_FILE.temp" "$STATE_FILE"
    echo -e "\033[0;32mDownloaded $STATE_FILE\033[0m"
fi

# Download header file if missing
if [ ! -f "$HEADER_FILE" ]; then
    echo -e "\033[0;33mMissing $HEADER_FILE. Downloading...\033[0m"
    wget -O "$HEADER_FILE.temp" https://media.githubusercontent.com/media/gnosischain/reth-init-state/refs/heads/main/chiado/header_700000.rlp
    mv "$HEADER_FILE.temp" "$HEADER_FILE"
    echo -e "\033[0;32mDownloaded $HEADER_FILE\033[0m"
fi

echo -e "\033[0;32mFiles downloaded [state & header]\033[0m"