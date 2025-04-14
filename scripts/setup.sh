#!/bin/bash

DOCKER=false

while [[ "$#" -gt 0 ]]; do
    case $1 in
        --datadir)
            INPUT_DIR="$2"
            shift 2
            ;;
        --chain)
            CHAIN="$2"
            shift 2
            ;;
        --docker)
            DOCKER=true
            shift
            ;;
        *)
            echo "Unknown parameter passed: $1"
            exit 1
            ;;
    esac
done

if [[ -z "$INPUT_DIR" ]]; then
    echo "Error: --datadir not specified"
    exit 1
fi

if [[ -z "$CHAIN" ]]; then
    echo "Error: --chain not specified"
    exit 1
fi

# Convert to full path
DATA_DIR="$(realpath "$INPUT_DIR")"
if [[ ! -d "$DATA_DIR" ]]; then
    echo "Error: DATA_DIR does not exist: $DATA_DIR"
    exit 1
fi

SCRIPT_DIR="$(dirname "$(realpath "$0")")"

echo -e "Chain:    \033[0;32m$CHAIN\033[0m"
echo -e "Data dir: \033[0;32m$DATA_DIR\033[0m\n"

# download the state and header files

# if chiado, run download-data-chiado.sh
if [[ "$CHAIN" == "chiado" ]]; then
    # Download files
    "$SCRIPT_DIR/download-data-chiado.sh" "$DATA_DIR"

    # Import state
    if [[ "$DOCKER" == true ]]; then
        "$SCRIPT_DIR/import-chiado-docker.sh" "$DATA_DIR"
    else
        "$SCRIPT_DIR/import-chiado.sh" "$DATA_DIR"
    fi
fi

# if gnosis, run download-data-gnosis.sh
if [[ "$CHAIN" == "gnosis" ]]; then
    # Download files
    "$SCRIPT_DIR/download-data-gnosis.sh" "$DATA_DIR"

    # Import state
    if [[ "$DOCKER" == true ]]; then
        "$SCRIPT_DIR/import-gnosis-docker.sh" "$DATA_DIR"
    else
        "$SCRIPT_DIR/import-gnosis.sh" "$DATA_DIR"
    fi
fi

echo -e "\n\033[0;32m###########\033[0m"
echo -e "\033[0;32m## Done! ##\033[0m"
echo -e "\033[0;32m###########\033[0m"