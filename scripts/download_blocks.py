import requests
from rlp import decode, encode
from web3 import Web3
from Crypto.Hash import keccak
from web3.types import HexBytes
import json


def get_block(n: int):
    url = "https://1rpc.io/gnosis"
    headers = {"Content-Type": "application/json"}
    data = {
        "method": "eth_getBlockByNumber",
        "params": [hex(n), True],
        "id": 1,
        "jsonrpc": "2.0",
    }
    response = requests.post(url, headers=headers, json=data)
    return response.json()["result"]


def convert_tx_to_rlp(tx):
    encoder_array = None

    if tx['type'] == '0x0':
        print(f"TX: {tx}")
        encoder_array = [
            int(tx["nonce"], 16),
            int(tx["gasPrice"], 16),
            int(tx["gas"], 16),
            Web3.to_bytes(hexstr=tx["to"]),
            int(tx["value"], 16),
            HexBytes(tx["input"]),
            int(tx["v"], 16),
            HexBytes(tx["r"]),
            HexBytes(tx["s"]),
        ]
        hash = keccak.new(digest_bits=256)
        hash.update(encode(encoder_array))
        print(f"TX HASH: {hash.hexdigest()}")

    if tx['type'] == '0x2':
        encoder_array = [
            int(tx["chainId"], 16),
            int(tx["nonce"], 16),
            int(tx["maxPriorityFeePerGas"], 16),
            int(tx["maxFeePerGas"], 16),
            int(tx["gas"], 16),
            Web3.to_bytes(hexstr=tx["to"]),
            int(tx["value"], 16),
            HexBytes(tx["input"]),    
            tx["accessList"],
            int(tx["v"], 16),
            HexBytes(tx["r"]),
            HexBytes(tx["s"]),
            # 0,
            # HexBytes("0x"),
            # HexBytes("0x"),
        ]

    if encoder_array is None:
        print(f"Tx type: {tx['type']}")
        raise TypeError("Custom Error: Invalid transaction type")

    encoded_rlp = encode(encoder_array)
    tx_rlp = encoded_rlp.hex()
    
    hash = keccak.new(digest_bits=256)
    hash.update(encoded_rlp)
    print(f"TX HASH: {hash.hexdigest()}")

    prefix = "0x"
    if tx['type'] != '0x0':
        prefix += f"0{int(tx['type'], 16)}"

    return f"{prefix}{tx_rlp}"


def save_converted_block_data(block_number):
    block = get_block(block_number)
    converted_block = {
        "baseFeePerGas": block["baseFeePerGas"],
        "blockHash": block["hash"],
        "blockNumber": block["number"],
        "extraData": block["extraData"],
        "feeRecipient": block["miner"],
        "gasLimit": block["gasLimit"],
        "gasUsed": block["gasUsed"],
        "logsBloom": block["logsBloom"],
        "parentHash": block["parentHash"],
        "prevRandao": block["mixHash"],
        "receiptsRoot": block["receiptsRoot"],
        "stateRoot": block["stateRoot"],
        "timestamp": block["timestamp"],
        "transactions": []
    }

    for tx in block["transactions"]:
        converted_block["transactions"].append(convert_tx_to_rlp(tx))

    with open(f"blocks/block_{block_number}.json", "w") as f:
        json.dump(converted_block, f, indent=4)

def main():
    latest_block_number = 26478700
    count = 5
    for block_number in range(latest_block_number+1, latest_block_number+1+count):
        save_converted_block_data(block_number)

if __name__ == "__main__":
    main()
