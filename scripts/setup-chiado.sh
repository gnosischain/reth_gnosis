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

# Download files
./scripts/download-data-chiado.sh "$DATA_DIR"

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