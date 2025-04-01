#!/bin/bash
set -e

CLEAR_FLAG=false
# Parse flags
for arg in "$@"; do
  case $arg in
    --clear)
      CLEAR_FLAG=true
      shift
      ;;
  esac
done

# first input or $PWD/data
DATA_DIR=$PWD/${1:-data}

echo "CLEAR_FLAG: $CLEAR_FLAG"

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
if [ ! -f $IMPORT_SUCCESS_FILE ] || [ $CLEAR_FLAG = true ]; then
    echo "Dropping existing database..."
    ./target/debug/reth --chain ./scripts/chainspecs/chiado.json db drop -f || true

    echo "Importing the state"
    ./target/debug/reth --chain ./scripts/chainspecs/chiado.json init-state $STATE_FILE --without-evm --header $HEADER_FILE --total-difficulty 231708131825107706987652208063906496124457284 --header-hash cdc424294195555e53949b6043339a49b049b48caa8d85bc7d5f5d12a85964b6
    touch $IMPORT_SUCCESS_FILE
fi

echo "2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a" > $DATA_DIR/jwtsecret
echo -e "\033[0;32mSetup complete\033[0m"