#!/bin/bash
set -euo pipefail

cleanup() {
    echo -e "\033[0;31mInterrupted. Cleaning up temp files...\033[0m"
    rm -f "$DATA_DIR/chunk_"*".part"
    rm -f "$STATE_FILE.part"
    exit 1
}
trap cleanup INT TERM

# first input or $PWD/data
DATA_DIR=$1

find "$DATA_DIR" -name '*.part' -delete

echo -e "\033[0;34mTrying to download files...\033[0m"

get_file_size() {
  if stat --version >/dev/null 2>&1; then
    # GNU (Linux)
    stat -c %s "$1"
  else
    # BSD/macOS
    stat -f %z "$1"
  fi
}

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
    FILE_SIZE=$(get_file_size "$OUTPUT_FILE")
    if [[ "$FILE_SIZE" -eq "$SIZE" ]]; then
      echo -e "\033[0;32m$OUTPUT_FILE already downloaded and complete. Skipping...\033[0m"
      continue
    else
      echo -e "\033[0;31m$OUTPUT_FILE is incomplete. Re-downloading...\033[0m"
      rm "$OUTPUT_FILE"
    fi
  fi

  echo -e "\033[0;34mRequesting download URL for $CHUNK_FILE...\033[0m"

  # Request download URL
  RESPONSE=$(curl -s -X POST \
    -H "Accept: application/vnd.git-lfs+json" \
    -H "Content-type: application/json" \
    -d "{\"operation\": \"download\", \"transfer\": [\"basic\"], \"objects\": [{\"oid\": \"$OID\", \"size\": $SIZE}]}" \
    "$LFS_URL")

  # Extract download URL
  DOWNLOAD_URL=$(echo "$RESPONSE" | jq -r .objects[0].actions.download.href)

  if [[ -z "$DOWNLOAD_URL" || "$DOWNLOAD_URL" == "null" ]]; then
    echo -e "\033[0;31mError: Could not retrieve download URL for $CHUNK_FILE\033[0m"
    continue
  fi

  echo -e "\033[0;34mDownloading $CHUNK_FILE...\033[0m"

  TEMP_FILE="$OUTPUT_FILE.part"
  wget --tries=3 -O "$TEMP_FILE" "$DOWNLOAD_URL"
  mv "$TEMP_FILE" "$OUTPUT_FILE"

  # Verify file size after download
  if [[ -f "$OUTPUT_FILE" ]]; then
    FILE_SIZE=$(get_file_size "$OUTPUT_FILE")
    if [[ "$FILE_SIZE" -eq "$SIZE" ]]; then
      echo -e "\033[0;32m$CHUNK_FILE downloaded successfully.\033[0m"
    else
      echo -e "\033[0;33mWarning: $CHUNK_FILE may be incomplete. Expected $SIZE bytes but got $FILE_SIZE bytes.\033[0m"
    fi
  fi
done

echo -e "\033[0;32mAll state chunks downloaded!\033[0m"

HEADER_FILE="$DATA_DIR/header_26478650.rlp"
if [[ ! -f "$HEADER_FILE" ]]; then
  TEMP_HEADER_FILE="$HEADER_FILE.part"
  wget https://media.githubusercontent.com/media/gnosischain/reth-init-state/refs/heads/main/gnosis/header_26478650.rlp -O "$TEMP_HEADER_FILE"
  mv "$TEMP_HEADER_FILE" "$HEADER_FILE"
  echo -e "\033[0;32mDownloaded $HEADER_FILE\033[0m"
fi

STATE_FILE="$DATA_DIR/state_26478650.jsonl"
STATE_SIZE=27498292407

for i in {0..6}; do
  CHUNK="$DATA_DIR/chunk_0$i"
  if [[ ! -f "$CHUNK" ]]; then
    echo -e "\033[0;31mError: Missing chunk $CHUNK, cannot combine. Re-run script.\033[0m"
    exit 1
  fi
done

# Check if file exists and has the correct size
if [[ -f "$STATE_FILE" ]]; then
  FILE_SIZE=$(get_file_size "$STATE_FILE")
  if [[ "$FILE_SIZE" -eq "$STATE_SIZE" ]]; then
    echo -e "\033[0;32mState already combined!\033[0m"
  else
    echo -e "\033[0;34mCombining files to state...\033[0m"
    rm "$STATE_FILE"
    cat "$(printf "$DATA_DIR/chunk_%02d " {0..6})" > "$STATE_FILE.part" && mv "$STATE_FILE.part" "$STATE_FILE"
    echo -e "\033[0;32mState file combined!\033[0m"
  fi
fi

FINAL_SIZE=$(get_file_size "$STATE_FILE")
if [[ "$FINAL_SIZE" -ne "$STATE_SIZE" ]]; then
  echo -e "\033[0;31mError: Combined state file size mismatch!\033[0m"
  exit 1
fi

echo -e "\033[0;32mFiles downloaded [state & header]\033[0m"