use std::sync::{Arc, OnceLock};

use core::{
    fmt::{self, Display, Formatter},
    str::FromStr,
};

use alloy_consensus::Header;
use alloy_genesis::Genesis;
use derive_more::{Constructor, Deref, From, Into};
use reth_chainspec::{
    BaseFeeParams, ChainHardforks, ChainSpec, ChainSpecBuilder, DepositContract, EthChainSpec,
    EthereumHardfork, EthereumHardforks, ForkCondition, ForkFilter, ForkFilterKey, ForkHash,
    ForkId, Hardfork, Hardforks, Head,
};
use reth_cli::chainspec::{parse_genesis, ChainSpecParser};
use reth_ethereum_forks::hardfork;
use reth_network_peers::{parse_nodes, NodeRecord};
use revm_primitives::{b256, B256, U256};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

const GNOSIS_NODES: &[&str] = &[
    "enode://fb14d72321ee823fcf21e163091849ee42e0f6ac0cddc737d79e324b0a734c4fc51823ef0a96b749c954483c25e8d2e534d1d5fc2619ea22d58671aff96f5188@65.109.103.148:30303",
    "enode://40f40acd78004650cce57aa302de9acbf54becf91b609da93596a18979bb203ba79fcbee5c2e637407b91be23ce72f0cc13dfa38d13e657005ce842eafb6b172@65.109.103.149:30303",
    "enode://9e50857aa48a7a31bc7b46957e8ced0ef69a7165d3199bea924cb6d02b81f1f35bd8e29d21a54f4a331316bf09bb92716772ea76d3ef75ce027699eccfa14fad@141.94.97.22:30303",
    "enode://96dc133ce3aeb5d9430f1dce1d77a36418c8789b443ae0445f06f73c6b363f5b35c019086700a098c3e6e54974d64f37e97d72a5c711d1eae34dc06e3e00eed5@141.94.97.74:30303",
    "enode://516cbfbe9bbf26b6395ed68b24e383401fc33e7fe96b9d235ebca86c9f812fde8d33a7dbebc0fb5595459d2c5cc6381595d96507af89e6b48b5bdd0ebf8af0c0@141.94.97.84:30303",
    "enode://fc86a93545c56322dd861180b76632b9baeb65af8f304269b489b4623ae060847569c3c3c10c4b39baf221a2cdefea66efabce061a542cdcda374cbba45aa3d4@51.68.39.206:30303",
    "enode://0e6dd3815a627893515465130c1e95aa73b18fe2f723b2467f3abf94df9be036f27595f301b5e78750ad128e59265f980c92033ae903330c0460c40ae088c04a@35.210.37.245:30303",
    "enode://b72d6233d50bef7b31c09f3ea39459257520178f985a872bbaa4e371ed619455b7671053ffe985af1b5fb3270606e2a49e4e67084debd75e6c9b93e227c5b01c@35.210.156.59:30303",
];

hardfork!(
    /// The name of an gnosis hardfork.
    ///
    /// When building a list of hardforks for a chain, it's still expected to mix with
    /// [`EthereumHardfork`].
    GnosisHardfork {
        ConstantinopleFix,
        POSDAOActivation,
    }
);

/// Chain spec builder for gnosis chain.
#[derive(Debug, Default, From)]
pub struct GnosisChainSpecBuilder {
    /// [`ChainSpecBuilder`]
    _inner: ChainSpecBuilder,
}

/// Gnosis chain spec type.
#[derive(Debug, Clone, Deref, Into, Constructor, PartialEq, Eq)]
pub struct GnosisChainSpec {
    /// [`ChainSpec`].
    pub inner: ChainSpec,
}

impl EthChainSpec for GnosisChainSpec {
    type Header = Header;

    fn chain(&self) -> alloy_chains::Chain {
        self.inner.chain()
    }

    fn base_fee_params_at_block(&self, block_number: u64) -> BaseFeeParams {
        self.inner.base_fee_params_at_block(block_number)
    }

    fn base_fee_params_at_timestamp(&self, timestamp: u64) -> BaseFeeParams {
        self.inner.base_fee_params_at_timestamp(timestamp)
    }

    fn deposit_contract(&self) -> Option<&DepositContract> {
        self.inner.deposit_contract()
    }

    fn genesis_hash(&self) -> B256 {
        self.inner.genesis_hash()
    }

    fn prune_delete_limit(&self) -> usize {
        self.inner.prune_delete_limit()
    }

    fn display_hardforks(&self) -> Box<dyn Display> {
        Box::new(ChainSpec::display_hardforks(self))
    }

    fn genesis_header(&self) -> &Self::Header {
        self.inner.genesis_header()
    }

    fn genesis(&self) -> &Genesis {
        self.inner.genesis()
    }

    fn max_gas_limit(&self) -> u64 {
        self.inner.max_gas_limit()
    }

    fn bootnodes(&self) -> Option<Vec<NodeRecord>> {
        // self.inner.bootnodes()
        Some(parse_nodes(GNOSIS_NODES))
    }
}

impl Hardforks for GnosisChainSpec {
    fn fork<H: reth_chainspec::Hardfork>(&self, fork: H) -> reth_chainspec::ForkCondition {
        self.inner.fork(fork)
    }

    fn forks_iter(
        &self,
    ) -> impl Iterator<Item = (&dyn reth_chainspec::Hardfork, reth_chainspec::ForkCondition)> {
        self.inner.forks_iter()
    }

    fn fork_id(&self, head: &Head) -> ForkId {
        let mut forkhash = ForkHash::from(b256!(
            "4f1dd23188aab3a76b463e4af801b52b1248ef073c648cbdc4c9333d3da79756"
        ));
        let mut current_applied = 0;

        // handle all block forks before handling timestamp based forks. see: https://eips.ethereum.org/EIPS/eip-6122
        for (_, cond) in self.hardforks.forks_iter() {
            // handle block based forks and the sepolia merge netsplit block edge case (TTD
            // ForkCondition with Some(block))
            if let ForkCondition::Block(block)
            | ForkCondition::TTD {
                fork_block: Some(block),
                ..
            } = cond
            {
                if cond.active_at_head(head) {
                    if block != current_applied {
                        forkhash += block;
                        current_applied = block;
                    }
                } else {
                    // we can return here because this block fork is not active, so we set the
                    // `next` value
                    return ForkId {
                        hash: forkhash,
                        next: block,
                    };
                }
            }
        }

        // timestamp are ALWAYS applied after the merge.
        //
        // this filter ensures that no block-based forks are returned
        for timestamp in self.hardforks.forks_iter().filter_map(|(_, cond)| {
            cond.as_timestamp()
                .filter(|time| time > &self.genesis.timestamp)
        }) {
            let cond = ForkCondition::Timestamp(timestamp);
            if cond.active_at_head(head) {
                if timestamp != current_applied {
                    forkhash += timestamp;
                    current_applied = timestamp;
                }
            } else {
                // can safely return here because we have already handled all block forks and
                // have handled all active timestamp forks, and set the next value to the
                // timestamp that is known but not active yet
                return ForkId {
                    hash: forkhash,
                    next: timestamp,
                };
            }
        }

        ForkId {
            hash: forkhash,
            next: 0,
        }
    }

    fn latest_fork_id(&self) -> ForkId {
        self.inner.latest_fork_id()
    }

    fn fork_filter(&self, head: Head) -> ForkFilter {
        let forks = self.hardforks.forks_iter().filter_map(|(_, condition)| {
            // We filter out TTD-based forks w/o a pre-known block since those do not show up in the
            // fork filter.
            Some(match condition {
                ForkCondition::Block(block)
                | ForkCondition::TTD {
                    fork_block: Some(block),
                    ..
                } => ForkFilterKey::Block(block),
                ForkCondition::Timestamp(time) => ForkFilterKey::Time(time),
                _ => return None,
            })
        });

        ForkFilter::new(
            head,
            b256!("4f1dd23188aab3a76b463e4af801b52b1248ef073c648cbdc4c9333d3da79756"),
            self.genesis_timestamp(),
            forks,
        )
    }
}

impl EthereumHardforks for GnosisChainSpec {
    fn get_final_paris_total_difficulty(&self) -> Option<U256> {
        self.inner.get_final_paris_total_difficulty()
    }

    fn final_paris_total_difficulty(&self, block_number: u64) -> Option<U256> {
        self.inner.final_paris_total_difficulty(block_number)
    }
}

impl From<Genesis> for GnosisChainSpec {
    fn from(genesis: Genesis) -> Self {
        // Block-based hardforks
        let hardfork_opts = [
            (
                EthereumHardfork::Homestead.boxed(),
                genesis.config.homestead_block,
            ),
            (EthereumHardfork::Dao.boxed(), genesis.config.dao_fork_block),
            (
                EthereumHardfork::Tangerine.boxed(),
                genesis.config.eip150_block,
            ),
            (
                EthereumHardfork::SpuriousDragon.boxed(),
                genesis.config.eip155_block,
            ),
            (
                EthereumHardfork::Byzantium.boxed(),
                genesis.config.byzantium_block,
            ),
            (
                EthereumHardfork::Constantinople.boxed(),
                genesis.config.constantinople_block,
            ),
            (GnosisHardfork::ConstantinopleFix.boxed(), Some(2508800)),
            (GnosisHardfork::POSDAOActivation.boxed(), Some(9186425)),
            (
                EthereumHardfork::Petersburg.boxed(),
                genesis.config.petersburg_block,
            ),
            (
                EthereumHardfork::Istanbul.boxed(),
                genesis.config.istanbul_block,
            ),
            // (EthereumHardfork::MuirGlacier.boxed(), genesis.config.muir_glacier_block),
            (
                EthereumHardfork::Berlin.boxed(),
                genesis.config.berlin_block,
            ),
            (
                EthereumHardfork::London.boxed(),
                genesis.config.london_block,
            ),
        ];
        let mut hardforks = hardfork_opts
            .into_iter()
            .filter_map(|(hardfork, opt)| opt.map(|block| (hardfork, ForkCondition::Block(block))))
            .collect::<Vec<_>>();

        // Paris
        let paris_block_and_final_difficulty =
            if let Some(ttd) = genesis.config.terminal_total_difficulty {
                hardforks.push((
                    EthereumHardfork::Paris.boxed(),
                    ForkCondition::TTD {
                        total_difficulty: ttd,
                        fork_block: genesis.config.merge_netsplit_block,
                    },
                ));

                genesis
                    .config
                    .merge_netsplit_block
                    .map(|block| (block, ttd))
            } else {
                None
            };

        // Time-based hardforks
        let time_hardfork_opts = [
            (
                EthereumHardfork::Shanghai.boxed(),
                genesis.config.shanghai_time,
            ),
            (EthereumHardfork::Cancun.boxed(), genesis.config.cancun_time),
            (EthereumHardfork::Prague.boxed(), genesis.config.prague_time),
            (EthereumHardfork::Osaka.boxed(), genesis.config.osaka_time),
        ];

        let mut time_hardforks = time_hardfork_opts
            .into_iter()
            .filter_map(|(hardfork, opt)| {
                opt.map(|time| (hardfork, ForkCondition::Timestamp(time)))
            })
            .collect::<Vec<_>>();

        hardforks.append(&mut time_hardforks);

        // Ordered Hardforks
        let mainnet_hardforks: ChainHardforks = EthereumHardfork::mainnet().into();
        let mainnet_order = mainnet_hardforks.forks_iter();

        let mut ordered_hardforks = Vec::with_capacity(hardforks.len());
        for (hardfork, _) in mainnet_order {
            if let Some(pos) = hardforks.iter().position(|(e, _)| **e == *hardfork) {
                ordered_hardforks.push(hardforks.remove(pos));
            }
        }

        // append the remaining unknown hardforks to ensure we don't filter any out
        ordered_hardforks.append(&mut hardforks);

        // NOTE: in full node, we prune all receipts except the deposit contract's. We do not
        // have the deployment block in the genesis file, so we use block zero. We use the same
        // deposit topic as the mainnet contract if we have the deposit contract address in the
        // genesis json.
        let deposit_contract =
            genesis
                .config
                .deposit_contract_address
                .map(|address| DepositContract {
                    address,
                    block: 0,
                    topic: b256!(
                        "649bbc62d0e31342afea4e5cd82d4049e7e1ee912fc0889aa790803be39038c5"
                    ),
                });

        Self {
            inner: ChainSpec {
                chain: genesis.config.chain_id.into(),
                genesis,
                genesis_hash: OnceLock::new(),
                hardforks: ChainHardforks::new(ordered_hardforks),
                paris_block_and_final_difficulty,
                deposit_contract,
                ..Default::default()
            },
        }
    }
}

/// Gnosis chain specification parser.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct GnosisChainSpecParser;

impl ChainSpecParser for GnosisChainSpecParser {
    type ChainSpec = GnosisChainSpec;

    const SUPPORTED_CHAINS: &'static [&'static str] = &["chiado", "gnosis"];

    fn parse(s: &str) -> eyre::Result<Arc<Self::ChainSpec>> {
        chain_value_parser(s)
    }
}

/// Clap value parser for [`GnosisChainSpec`]s.
///
/// The value parser matches either a known chain, the path
/// to a json file, or a json formatted string in-memory. The json needs to be a Genesis struct.
pub fn chain_value_parser(s: &str) -> eyre::Result<Arc<GnosisChainSpec>, eyre::Error> {
    Ok(match s {
        // currently it's mandatory to specify the path to the chainspec file
        // TODO: allow for hardcoded built-in chains
        _ => Arc::new(parse_genesis(s)?.into()),
    })
}
