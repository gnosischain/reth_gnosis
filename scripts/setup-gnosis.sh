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

echo -e "State directory: \033[0;32m$DATA_DIR\033[0m"
echo -e "(This is where the state files will be downloaded and imported)\n"

# Download files
./scripts/download-data-gnosis.sh "$DATA_DIR"

IMPORT_SUCCESS_FILE=$DATA_DIR/import_success

# if import success file is missing, import the state
if [ ! -f $IMPORT_SUCCESS_FILE ] || [ $CLEAR_FLAG = true ]; then
  echo "Dropping existing database..."
  ./target/debug/reth --chain ./scripts/chainspecs/mainnet.json db drop -f || true

  # Run the reth command and check if it succeeds before creating the success file
  if ./target/debug/reth --chain ./scripts/chainspecs/mainnet.json init-state $STATE_FILE --without-evm --header $HEADER_FILE --total-difficulty 8626000110427540000000000000000000000000000000 --header-hash a133198478cb01b4585604d07f584633f1f147103b49672d2bd87a5a3ba2c06e; then
    touch $IMPORT_SUCCESS_FILE  # Only create the file if the command was successful
  else
    echo "State import failed. $IMPORT_SUCCESS_FILE will not be created."
  fi
fi

echo "2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a" > $DATA_DIR/jwtsecret
echo -e "\033[0;32mSetup complete\033[0m"