# Reth @ Gnosis 🍴

Gnosis compatible Reth client. Not a fork, but an extension with the `NodeBuilder` API.

Refer to the Reth's documentation to run a node: https://reth.rs/

## Implementation progress

- [ ] Pre-merge POSDAO / AuRa
- [x] [EIP-1559 modifications](https://github.com/gnosischain/specs/blob/master/network-upgrades/london.md)
- [x] [Post-merge POSDAO](https://github.com/gnosischain/specs/blob/master/execution/posdao-post-merge.md)
- [x] [Gnosis withdrawals](https://github.com/gnosischain/specs/blob/master/execution/withdrawals.md)


# Installation for Chiado

## Option 1: Build from source

Currently the recommended way of running reth is by building it from source. To do so, you need to have Rust installed. You can install it by following the instructions on the [official website](https://www.rust-lang.org/tools/install).

After installing Rust, you can clone the repository and build the project by running the following commands:

```bash
git clone https://github.com/gnosischain/reth_gnosis.git
cd reth_gnosis
git checkout pectra-alphas

cargo build
```

This will build the project in debug mode.

### Node setup

This is the step where Reth differs from other clients. You need to import the state at merge since we don't support the pre-merge block format yet. To do so, you need to download the state and the header at the last block (till which you're importing the state). All this is taken care of by the `setup-chiado.sh` script. You can run it by running the following command:

```bash
./scripts/setup-chiado.sh --clear
```

> **Note**: If the above script fails for any given reason (such as SSH disconnect, background PID killed, etc.), you have to re-run it mandatorily passing the --clear flag. This is because, if the db isn't initialized properly (i.e. the full import pipeline didn't run), the node cannot sync.

### Running the node

After setting up the node, you can run it by running the following command:

```ocaml
./target/debug/reth node \
    -vvvv \
    --chain ./scripts/chainspecs/chiado.json \
    --http \
    --http.port=8545 \
    --http.addr=0.0.0.0 \
    --http.corsdomain='*' \
    --http.api=admin,net,eth,web3,debug,trace \
    --authrpc.port=8546 \
    --authrpc.jwtsecret=./scripts/networkdata/jwtsecret \
    --authrpc.addr=0.0.0.0
```

You can specify a data directory by adding the `--datadir` flag.  
You can see the default data directory using:

```bash
./target/debug/reth db path
```

> **Note:** This version of reth_gnosis is only for internal testing and is not recommended for production use.  
> Please do not use it for validating purposes.

## Option 2: Docker image

You can also build the Docker image yourself and run it.

```bash
git clone https://github.com/gnosischain/reth_gnosis.git
cd reth_gnosis
git checkout chiado-pectra

docker build -t reth .
```

After building the image, you need to set the node up for the same reason as mentioned above.  
You can do so by running the following command:

### Docker setup

```bash
./scripts/docker-setup-chiado.sh --clear
```
Optionally, you can specify the data directory by specifying it like `./scripts/docker-setup-chiado.sh /path/to/data`.

> **Note**: If the above script fails for any given reason (such as SSH disconnect, background PID killed, etc.), you have to re-run it mandatorily passing the --clear flag. This is because, if the db isn't initialized properly (i.e. the full import pipeline didn't run), the node cannot sync.

### Running the node

Now it's ready to run the node.  
You can run it by running the following command:

```bash
DATA_DIR=$(pwd)/data
docker run --network host \
    -v $DATA_DIR:/data \
    reth node \
    -vvvv \
    --chain chainspecs/chiado.json \
    --datadir /data \
    --http \
    --http.port=8545 \
    --http.addr=0.0.0.0 \
    --http.corsdomain='*' \
    --http.api=admin,net,eth,web3,debug,trace \
    --authrpc.port=8546 \
    --authrpc.jwtsecret=/data/jwtsecret \
    --authrpc.addr=0.0.0.0 \
    --discovery.port=30303 \
    --discovery.addr=0.0.0.0
```

Here, the `$DATA_DIR` is the directory where you downloaded the state data in `./scripts/docker-setup-chiado.sh`. By default, it is `./data`.

# Installation for Mainnet

The same steps apply for the mainnet, but you need to use the mainnet and mainnet file downloader.

## Option 1: Build from source

### Node setup
You can set up the node by running the following command:

```bash
./scripts/setup-gnosis.sh --clear
```

### Running the node
After setting up the node, you can run it by running the following command:

```ocaml
./target/debug/reth node \
    -vvvv \
    --chain ./scripts/chainspecs/gnosis.json \
    --http \
    --http.port=8545 \
    --http.addr=0.0.0.0 \
    --http.corsdomain='*' \
    --http.api=admin,net,eth,web3,debug,trace \
    --authrpc.port=8546 \
    --authrpc.jwtsecret=./scripts/networkdata/jwtsecret \
    --authrpc.addr=0.0.0.0
```

## Option 2: Docker image

### Docker setup
You can set up the node by running the following command:

```bash
./scripts/docker-setup-gnosis.sh --clear
```

### Running the node
After setting up the node, you can run it by running the following command:

```bash
DATA_DIR=$(pwd)/data
docker run --network host \
    -v $DATA_DIR:/data \
    reth node \
    -vvvv \
    --chain chainspecs/gnosis.json \
    --datadir /data \
    --http \
    --http.port=8545 \
    --http.addr=0.0.0.0 \
    --http.corsdomain='*' \
    --http.api=admin,net,eth,web3,debug,trace \
    --authrpc.port=8546 \
    --authrpc.jwtsecret=/data/jwtsecret \
    --authrpc.addr=0.0.0.0 \
    --discovery.port=30303 \
    --discovery.addr=0.0.0.0
```