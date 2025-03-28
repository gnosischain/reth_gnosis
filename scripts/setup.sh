#!/bin/bash
set -e

# first input or $PWD/data
DATA_DIR=$PWD/${1:-data}

echo -e "State directory: \033[0;32m$DATA_DIR\033[0m"
echo -e "(This is where the state files will be downloaded and imported)\n"

STATE_FILE=$DATA_DIR/state_at_700000.jsonl
HEADER_FILE=$DATA_DIR/header_700000.rlp
IMPORT_SUCCESS_FILE=$DATA_DIR/import_success
DOWNLOAD_SUCCESS_FILE=$DATA_DIR/download_success

# if either of the file is missing, delete the data directory, and download the files
if [ ! -f $STATE_FILE ] || [ ! -f $HEADER_FILE ] || [ ! -f $DOWNLOAD_SUCCESS_FILE ]; then
    echo "Either $STATE_FILE or $HEADER_FILE is missing. Deleting the data directory and downloading the files."
    rm -rf $DATA_DIR
    mkdir -p $DATA_DIR
    wget https://media.githubusercontent.com/media/gnosischain/reth-init-state/refs/heads/main/chiado/state_700000.jsonl -O $STATE_FILE
    wget https://media.githubusercontent.com/media/gnosischain/reth-init-state/refs/heads/main/chiado/header_700000.rlp -O $HEADER_FILE
    touch $DOWNLOAD_SUCCESS_FILE
fi

# if import success file is missing, import the state
if [ ! -f $IMPORT_SUCCESS_FILE ]; then
    echo "Dropping existing database..."
    yes | ./target/debug/reth --chain ./scripts/chiado_chainspec.json db drop || true

    echo "Importing the state"
    ./target/debug/reth --chain ./scripts/chiado_chainspec.json init-state $STATE_FILE --without-evm --header $HEADER_FILE --total-difficulty 231708131825107706987652208063906496124457284 --header-hash 08cf5eed684e84eccb9809d1d8de287b0bfad27e735c60e98709ab060106b04c
    touch $IMPORT_SUCCESS_FILE
fi

echo -e "\033[0;32mSetup complete\033[0m"