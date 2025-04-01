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

# GitHub organization and repository
ORG="gnosischain"
REPO="reth-init-state"

# Base Git LFS URL
LFS_URL="https://github.com/$ORG/$REPO.git/info/lfs/objects/batch"

# Hardcoded OIDs and sizes
OIDS=(
  "cd3b4b0edc6fc86bd9eee682ed0c6a1cc9ddc90fde12c855f960baf6ad74f11b"
  "3c591add3562c42baa113623418bb6f51fb73f183a866a30a372be52206d54c3"
  "4a7be543b8c2bd00e4a2b51ae35e065c29ddbb38becb62c42199a15d56f0d432"
  "c8ea30f3b2a065485cd568ae384f80abdb970ed99cf46666e106a613e7903743"
  "db2a3aa71490295a9de55c80fcb8097981079c5acedb9fc01aebdf9a0fd7d480"
  "eeec94bee7c49f0c2de2d2bf608d96ac0e870f9819e53edd738fff8467bde6ad"
  "ad2ecfba180f5da124d342134f766c4ab90280473e487f7f3eb73d19bf7598b1"
)

SIZES=(
  4294967296
  4294967296
  4294967296
  4294967296
  4294967296
  4294967296
  1728488631
)

# Loop through chunks 00 to 06
for i in {0..6}; do
  CHUNK_FILE="chunk_0$i"
  OID="${OIDS[$i]}"
  SIZE="${SIZES[$i]}"

  OUTPUT_FILE="$DATA_DIR/$CHUNK_FILE"

  # Check if file exists and has the correct size
  if [[ -f "$OUTPUT_FILE" ]]; then
    FILE_SIZE=$(stat -c %s "$OUTPUT_FILE")
    if [[ "$FILE_SIZE" -eq "$SIZE" ]]; then
      echo "$OUTPUT_FILE already downloaded and complete. Skipping..."
      continue
    else
      echo "$OUTPUT_FILE is incomplete. Re-downloading..."
      rm "$OUTPUT_FILE"
    fi
  fi

  echo "Requesting download URL for $CHUNK_FILE..."

  # Request download URL
  RESPONSE=$(curl -s -X POST \
    -H "Accept: application/vnd.git-lfs+json" \
    -H "Content-type: application/json" \
    -d "{\"operation\": \"download\", \"transfer\": [\"basic\"], \"objects\": [{\"oid\": \"$OID\", \"size\": $SIZE}]}" \
    "$LFS_URL")

  # Extract download URL
  DOWNLOAD_URL=$(echo "$RESPONSE" | jq -r .objects[0].actions.download.href)

  if [[ -z "$DOWNLOAD_URL" || "$DOWNLOAD_URL" == "null" ]]; then
    echo "Error: Could not retrieve download URL for $CHUNK_FILE"
    continue
  fi

  echo "Downloading $CHUNK_FILE..."
  wget -O "$OUTPUT_FILE" "$DOWNLOAD_URL"

  # Verify file size after download
  if [[ -f "$OUTPUT_FILE" ]]; then
    FILE_SIZE=$(stat -c %s "$OUTPUT_FILE")
    if [[ "$FILE_SIZE" -eq "$SIZE" ]]; then
      echo "$CHUNK_FILE downloaded successfully."
    else
      echo "Warning: $CHUNK_FILE may be incomplete. Expected $SIZE bytes but got $FILE_SIZE bytes."
    fi
  fi
done

echo "All files downloaded!"

HEADER_FILE="$DATA_DIR/header_26478650.rlp"
if [[ -f "$HEADER_FILE" ]]; then
  wget https://media.githubusercontent.com/media/gnosischain/reth-init-state/refs/heads/main/gnosis/header_26478650.rlp -O $HEADER_FILE
fi

STATE_FILE="$DATA_DIR/state_26478650.jsonl"
STATE_SIZE=27498292407

# Check if file exists and has the correct size
if [[ -f "$STATE_FILE" ]]; then
  FILE_SIZE=$(stat -c %s "$STATE_FILE")
  if [[ "$FILE_SIZE" -eq "$STATE_SIZE" ]]; then
    echo "State already combined!"
  else
    echo "Combining files to state"
    rm "$STATE_FILE"
    cat chunk_* > state_26478650.jsonl
    echo "State file combined!"
  fi
fi

echo "State file ready..."

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