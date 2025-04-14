# Reth @ Gnosis 🍴

Gnosis compatible Reth client. Not a fork, but an extension with the `NodeBuilder` API.

Refer to the Reth's documentation to run a node: https://reth.rs/

## Implementation progress

- [ ] Pre-merge POSDAO / AuRa
- [x] [EIP-4844-pectra](https://github.com/gnosischain/specs/blob/master/network-upgrades/pectra.md)
- [x] [EIP-1559 modifications](https://github.com/gnosischain/specs/blob/master/network-upgrades/london.md)
- [x] [Post-merge POSDAO](https://github.com/gnosischain/specs/blob/master/execution/posdao-post-merge.md)
- [x] [Gnosis withdrawals](https://github.com/gnosischain/specs/blob/master/execution/withdrawals.md)


# Installation

Reth differs from other clients, you need to import a post-merge state since we don't support the pre-merge yet. All file downloads are handled internally in the setup script.
You can run the node using two methods: Docker or building from source.

## Option 1: Using Docker

You can run the node using Docker. You can pull the image from the Docker Hub by running the following command:

```bash
docker pull ghcr.io/gnosischain/reth_gnosis:master
```

### Node Setup

You need to create a directory where you want all data files (state downloads, reth's database, configs, etc.) to be stored. For example, let's create `./reth_data` in the current folder.

```bash
mkdir ./reth_data
```

Now you can run the setup script using:

```bash
./scripts/setup.sh --datadir ./reth_data --chain chiado --docker
```

This configures the node for Chiado. You can use `--gnosis` for Gnosis Chain.

### Running the node

Before running the node, move your jwtsecret file to the `./reth_data` directory. You can run it by running the following command:

```bash
cp /path/to/jwtsecret ./reth_data/jwtsecret
```

```bash
docker run \
    -v ./reth_data:/data \
    ghcr.io/gnosischain/reth_gnosis:master node \
    --chain chainspecs/chiado.json \
    --datadir /data \
    --authrpc.jwtsecret=/data/jwtsecret
```

This runs Chiado, and you can use `chainspecs/gnosis.json` for Gnosis Chain. A full command (along with network and config) would look like this:

```bash
docker run --network host \
    -v $DATA_DIR:/data \
    ghcr.io/gnosischain/reth_gnosis:master node \
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

## Option 2: Build from source

Currently the recommended way of running reth is by building it from source. To do so, you need to have Rust installed. You can install it by following the instructions on the [official website](https://www.rust-lang.org/tools/install).

After installing Rust, you can clone the repository and build the project by running the following commands:

```bash
git clone https://github.com/gnosischain/reth_gnosis.git
cd reth_gnosis
git checkout pectra-alphas

cargo build
```

This will build the project in debug mode.

### Node Setup

You need to create a directory where you want all data files (state downloads, reth's database, configs, etc.) to be stored. For example, let's create `./reth_data` in the current folder.

```bash
mkdir ./reth_data
```

Now you can run the setup script (from inside `.../reth_gnosis`) using:

```bash
./scripts/setup.sh --datadir ./reth_data --chain chiado
```

This configures the node for Chiado. You can use `--gnosis` for Gnosis Chain.

### Running the node

Before running the node, move your jwtsecret file to the `./reth_data` directory. Now you can run it by running the following command:

```bash
cp /path/to/jwtsecret ./reth_data/jwtsecret
```

```bash
./target/debug/reth node \
    -vvvv \
    --chain ./scripts/chainspecs/chiado.json \
    --datadir ./reth_data \
    --http \
    --http.port=8545 \
    --http.addr=0.0.0.0 \
    --http.corsdomain='*' \
    --http.api=admin,net,eth,web3,debug,trace \
    --authrpc.port=8546 \
    --authrpc.jwtsecret=./reth_data/jwtsecret \
    --authrpc.addr=0.0.0.0 \
    --discovery.port=30303 \
    --discovery.addr=0.0.0.0
```

This runs Chiado, and you can use `./scripts/chainspecs/gnosis.json` for Gnosis Chain.