#!/bin/bash

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

# Download files
./scripts/download-data-gnosis.sh "$DATA_DIR"

IMPORT_SUCCESS_FILE=$DATA_DIR/import_success

# if import success file is missing, import the state
if [ ! -f $IMPORT_SUCCESS_FILE ] || [ $CLEAR_FLAG = true ]; then
  echo "Dropping existing database..."
  docker run -v $DATA_DIR:/data reth --chain chainspecs/mainnet.json db --datadir /data drop -f || true

  echo "Importing the state"
  docker run -v $DATA_DIR:/data reth --chain chainspecs/mainnet.json init-state /data/state_26478650.jsonl --without-evm --header /data/header_26478650.jsonl --total-difficulty 8626000110427540000000000000000000000000000000 --header-hash a133198478cb01b4585604d07f584633f1f147103b49672d2bd87a5a3ba2c06e --datadir /data
  echo "State import failed. $IMPORT_SUCCESS_FILE will not be created."
fi

echo "2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a" > $DATA_DIR/jwtsecret
echo -e "\033[0;32mSetup complete\033[0m"