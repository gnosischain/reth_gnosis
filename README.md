# Reth @ Gnosis 🍴

A [Reth](https://github.com/paradigmxyz/reth)-based execution client for **Gnosis Chain** and **Chiado**.

It is **not a fork** — it extends upstream Reth through the `NodeBuilder` API, adding Gnosis-specific
consensus (AuRa + POSDAO), withdrawals, fee handling, and hardforks on top of stock Reth.

| Network | Chain ID | `--chain` value |
| ------- | -------- | --------------- |
| Gnosis Chain (mainnet) | `100` | `gnosis` |
| Chiado (testnet) | `10200` | `chiado` |

> For general Reth flags and tuning, see the [Reth docs](https://reth.rs/).

---

## ⚡ Quick start

Pick the profile that matches how you run your node. Each line downloads a snapshot, then starts syncing.

```bash
# Validator (lightest disk footprint)
reth download --chain gnosis --minimal && reth node --chain gnosis --minimal

# DApp backend (recent history)
reth download --chain gnosis --full    && reth node --chain gnosis --full

# RPC provider / indexer (full archive)
reth download --chain gnosis --archive && reth node --chain gnosis
```

Swap `--chain gnosis` for `--chain chiado` to run the testnet. Add `--authrpc.jwtsecret=<path>` so your
consensus client can connect (see [Running the node](#running-the-node)).

> Don't want to download a snapshot? Just run `reth node --chain gnosis` — it will **sync from genesis**
> (see [Sync options](#sync-options)).

---

## Installation

### Option 1 — Docker

```bash
docker pull ghcr.io/gnosischain/reth_gnosis:master   # or a version tag, e.g. :v2.0.0
```

Mount a data directory and your `jwtsecret`, then run any subcommand by appending it to the image:

```bash
mkdir -p ./reth_data
cp /path/to/jwtsecret ./reth_data/jwtsecret

docker run --network host -v ./reth_data:/data \
    ghcr.io/gnosischain/reth_gnosis:master \
    node --chain chiado --datadir /data --authrpc.jwtsecret=/data/jwtsecret
```

### Option 2 — Build from source

Requires a [Rust toolchain](https://www.rust-lang.org/tools/install).

```bash
git clone https://github.com/gnosischain/reth_gnosis.git
cd reth_gnosis
cargo build --release
# binary at ./target/release/reth
```

The examples below assume `reth` is on your `PATH`; otherwise use `./target/release/reth`.

---

## Running the node

A minimal command needs a chain, a data directory, and a JWT secret for the Engine API:

```bash
reth node \
    --chain gnosis \
    --datadir ./reth_data \
    --authrpc.jwtsecret=./reth_data/jwtsecret
```

A fuller example with the HTTP-RPC and networking ports opened up:

```bash
reth node \
    -vvvv \
    --chain gnosis \
    --datadir ./reth_data \
    --http --http.port=8545 --http.addr=0.0.0.0 --http.corsdomain='*' \
    --http.api=admin,net,eth,web3,debug,trace \
    --authrpc.port=8546 --authrpc.addr=0.0.0.0 \
    --authrpc.jwtsecret=./reth_data/jwtsecret \
    --discovery.port=30303 --discovery.addr=0.0.0.0
```

> Like any execution client, `reth_gnosis` needs a **consensus client** (e.g. Lighthouse, Nimbus) pointed
> at its Engine API (`--authrpc.*`) to follow the chain.

### Node types

Choose how much historical data to keep. Match the flag you used at download time:

| Mode | Flag | Keeps | Best for |
| ---- | ---- | ----- | -------- |
| **Archive** *(default)* | *(none)* | All historical state & receipts | RPC providers, indexers, explorers |
| **Full** | `--full` | Recent state + full bodies | DApp backends, general use |
| **Minimal** | `--minimal` | Only what's needed to follow the tip | Validators, low-disk machines |

---

## Sync options

There are three ways to get a node to the chain tip. They are mutually exclusive — pick one.

### 1. Snapshot download (recommended)

Fetch a published snapshot, then start the node. Fastest path to a synced node, uses storage v2.

```bash
reth download --chain gnosis --minimal   # or --full / --archive
reth node     --chain gnosis --minimal
```

Snapshots are auto-discovered from `https://reth-snapshots.gnosischain.com`. To pin one manually, pass
`--manifest-url`, e.g. `https://reth-snapshots.gnosischain.com/latest/gnosis/manifest.json`. Browse the
available presets and components with `reth download --chain gnosis --help`.

### 2. Sync from genesis (default)

With no snapshot, the node executes every block since genesis using AuRa consensus. No flags needed:

```bash
reth node --chain gnosis --datadir ./reth_data --authrpc.jwtsecret=./reth_data/jwtsecret
```

This is the simplest setup and uses storage v2, but takes longer than a snapshot.

### 3. Post-merge state import (legacy)

The pre-v2 behavior: download a canonical post-merge state and import it before sync. Opt in with a flag.
**This forces the legacy storage-v1 layout** and is generally only needed for compatibility.

```bash
reth node --chain gnosis --gnosis.import-post-merge-state true
```

The import is idempotent — once it succeeds, an `imported.flag` file in the datadir makes later launches
skip the download regardless of the flag.

---

## CLI reference

`reth_gnosis` exposes Reth's CLI with Gnosis defaults. Common subcommands:

| Command | Description |
| ------- | ----------- |
| `reth node` | Start the node |
| `reth download` | Download a public snapshot (minimal / full / archive) |
| `reth import` | Import RLP-encoded blocks from a file |
| `reth import-era` | Import ERA-encoded blocks from a directory (default download url available for --chain=gnosis) |
| `reth export-era` | Export blocks to `era1` files |
| `reth init` / `reth init-state` | Initialize the DB from a genesis or state-dump file |
| `reth db` | Database inspection & maintenance utilities |
| `reth prune` | Prune according to the configured prune settings |
| `reth stage` | Run or unwind individual sync stages |
| `reth p2p` | P2P debugging utilities |
| `reth dump-genesis` | Print the genesis block config to stdout |
| `reth re-execute` | Re-execute blocks in parallel to verify historical sync |

Run `reth <command> --help` for the full set of flags. `--chain` (default `gnosis`) is accepted on every
command.

> ⚠️ **`reth db migrate-v2` is not supported** in `reth_gnosis` v2.0.0. Existing storage-v1 databases
> keep working as-is; to move to storage v2, re-sync with a snapshot download instead.

---

## Data directory

`--datadir` is optional but recommended. Without it, Reth uses the OS default:

- **Linux:** `$XDG_DATA_HOME/reth/` or `$HOME/.local/share/reth/`
- **macOS:** `$HOME/Library/Application Support/reth/`
- **Windows:** `{FOLDERID_RoamingAppData}/reth/`

---

## How Gnosis differs from Ethereum

`reth_gnosis` implements Gnosis Chain's consensus and execution rules on top of Reth:

- **Consensus:** AuRa (pre-merge) and POSDAO, till merge.
- **Withdrawals:** credited via the withdrawal/deposit contract, **not** minted as native token.
- **Block rewards:** the block-rewards contract mints bridged xDAI.
- **EIP-1559:** the base fee goes to a **fee collector** contract instead of being burned.
- **EIP-170:** the contract code-size limit activates at Shanghai (not Spurious Dragon).
- **Bytecode rewrites:** hardfork-triggered contract upgrades (e.g. the Balancer fork).

### Implemented

- [x] Pre-merge POSDAO / AuRa (genesis → Paris)
- [x] [Post-merge POSDAO](https://github.com/gnosischain/specs/blob/master/execution/posdao-post-merge.md)
- [x] [Gnosis withdrawals](https://github.com/gnosischain/specs/blob/master/execution/withdrawals.md)
- [x] [EIP-1559 modifications](https://github.com/gnosischain/specs/blob/master/network-upgrades/london.md)
- [x] [Pectra](https://github.com/gnosischain/specs/blob/master/network-upgrades/pectra.md)
- [x] [Fusaka](https://github.com/gnosischain/specs/blob/master/network-upgrades/fusaka.md)

---

## Development

```bash
cargo build              # debug build
cargo build --release    # release build
cargo check              # type-check
cargo fmt                # format
cargo clippy             # lint
cargo test --features testing               # run the test suite
cargo test --features testing <test_name>   # run a single test
```

Tests live in `src/testing/` and run the Ethereum / EEST spec-test vectors adapted for Gnosis. The
`testing` feature flag enables test compilation; known-incompatible vectors are gated behind the
`failing-tests` feature.

---

## Links

- 📦 [Releases](https://github.com/gnosischain/reth_gnosis/releases)
- 📖 [Reth documentation](https://reth.rs/)
- 📑 [Gnosis Chain specs](https://github.com/gnosischain/specs)
