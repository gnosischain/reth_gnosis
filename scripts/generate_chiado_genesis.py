import re
import json

# Parse .txt dump from gballet

input_allocs_file = "chiado_allocs_gballet.txt"
output_genesis_file = "chiado_genesis_alloc.json"


genesis = {
  "config": {
    "chainId": 10200,
    "consensus": "aura",
    "homesteadBlock": 0,
    "eip150Block": 0,
    "eip155Block": 0,
    "byzantiumBlock": 0,
    "constantinopleBlock": 0,
    "petersburgBlock": 0,
    "istanbulBlock": 0,
    "berlinBlock": 0,
    "londonBlock": 0,
    "burntContract": {
      "0": "0x1559000000000000000000000000000000000000"
    },
    "terminalTotalDifficulty": 0,
    "terminalTotalDifficultyPassed": True,
    "shanghaiTime": 1704401480,
    "cancunTime": 1704403000,
    "minBlobGasPrice": 1000000000,
    "maxBlobGasPerBlock": 262144,
    "targetBlobGasPerBlock": 131072,
    "blobGasPriceUpdateFraction": 1112826,
    "aura": {
      "stepDuration": 5,
      "blockReward": 0,
      "maximumUncleCountTransition": 0,
      "maximumUncleCount": 0,
      "validators": {
        "multi": {
          "0": {
            "list": [
              "0x14747a698Ec1227e6753026C08B29b4d5D3bC484"
            ]
          },
          "67334": {
            "list": [
                "0x14747a698Ec1227e6753026C08B29b4d5D3bC484",
                "0x56D421c0AC39976E89fa400d34ca6579417B84cA",
                "0x5CD99ac2F0F8C25a1e670F6BaB19D52Aad69D875",
                "0x60F1CF46B42Df059b98Acf67C1dD7771b100e124",
                "0x655e97bA0f63A56c2b56EB3e84f7bf42b20Bae14",
                "0x755B6259938D140626301c0B6026c1C00C9eD5d9",
                "0xa8010da9Cb0AC018C86A06301963853CC371a18c"
            ]
          }
        }
      },
      "blockRewardContractAddress": "0x2000000000000000000000000000000000000001",
      "blockRewardContractTransition": 0,
      "randomnessContractAddress": {
        "0": "0x3000000000000000000000000000000000000001"
      },
      "withdrawalContractAddress": "0xbabe2bed00000000000000000000000000000003",
      "twoThirdsMajorityTransition": 0,
      "posdaoTransition": 0,
      "blockGasLimitContractTransitions": {
        "0": "0x4000000000000000000000000000000000000001"
      },
      "registrar": "0x6000000000000000000000000000000000000000"
    },
    "eip1559collector": "0x1559000000000000000000000000000000000000"
  },
  "baseFeePerGas": "0x3b9aca00",
  "difficulty": "0x01",
  "gasLimit": "0x989680",
  "seal": {
    "authorityRound": {
      "step": "0x0",
      "signature": "0x0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
    }
  },
  "alloc": {}
}



with open(input_allocs_file, 'r') as file:
    data = file.read()


def remove_first_line(s):
    lines = s.split('\n')
    return '\n'.join(lines[1:])


def pop_first_line(s):
    lines = s.split('\n')
    return lines[0], '\n'.join(lines[1:])



sections = re.split("===== accounts", data)
# Remove the "===== storage slots =====" header
storage = remove_first_line(sections[0]).strip()
# Remove the first line (partial header "acounts + code =====")
accounts = remove_first_line(sections[1]).strip()

# Assign account properties first
for account in accounts.split('\n'):
    # parse account properties
    props = {}
    for keyvalue in re.split(r'\s|,\s', account.strip()):
        parts = keyvalue.split('=')
        key = parts[0]
        value = parts[1]
        props[key] = value
    # Assign to genesis
    account_addr = "0x" + props["addr"]
    if account_addr not in genesis["alloc"]:
        genesis["alloc"][account_addr] = {}
    genesis["alloc"][account_addr]["nonce"] = props["nonce"]
    genesis["alloc"][account_addr]["balance"] = props["balance"]
    genesis["alloc"][account_addr]["code"] = props["code"]

# Then parse storage slots (but they are defined first in the file)
for account in storage.split("\n\n"):
    # Split the first line as account address
    account_addr, slots = pop_first_line(account)
    storage_kv = {}

    for slot in slots.split('\n'):
        keyvalue = slot.split(" : ")
        key = keyvalue[0]
        value = keyvalue[1]
        storage_kv[key] = value

    if account_addr not in genesis["alloc"]:
        genesis["alloc"][account_addr] = {}
    genesis["alloc"][account_addr]["storage"] = storage_kv

with open(output_genesis_file, 'w') as file:
    json.dump(genesis, file, indent=2)
