use std::sync::Arc;

use core::fmt::Display;

use alloy_consensus::Header;
use alloy_eips::eip7840::BlobParams;
use alloy_genesis::Genesis;
use derive_more::{Constructor, Deref, From, Into};
use reth_chainspec::{
    make_genesis_header, BaseFeeParams, BaseFeeParamsKind, ChainHardforks, ChainSpec,
    ChainSpecBuilder, DepositContract, EthChainSpec, EthereumHardfork, EthereumHardforks,
    ForkCondition, ForkFilter, ForkFilterKey, ForkHash, ForkId, Hardfork, Hardforks, Head,
};
use reth_cli::chainspec::{parse_genesis, ChainSpecParser};
use reth_ethereum_forks::hardfork;
use reth_evm::eth::spec::EthExecutorSpec;
use reth_network_peers::{parse_nodes, NodeRecord};
use reth_primitives::SealedHeader;
use revm_primitives::{b256, Address, B256, U256};

#[derive(Debug, PartialEq, Eq)]
enum Chain {
    Gnosis,
    Chiado,
}

impl Chain {
    fn from_chain_id(chain_id: u64) -> Option<Self> {
        match chain_id {
            100 => Some(Chain::Gnosis),
            10200 => Some(Chain::Chiado),
            _ => None,
        }
    }
}

use crate::blobs::GNOSIS_BLOB_SCHEDULE;

use super::chains::{CHIADO_GENESIS, GNOSIS_GENESIS};

const GNOSIS_NODES: &[&str] = &[
    "enode://6765fff89db92aa8d923e28c438af626c8ae95a43093cdccbd6f550a7b6ce6ab5d1a3dc60dd79af3e6d2c2e6731bae629f0e54446a0d9da408c4eca7ebcd8485@3.75.159.31:30303",
    "enode://9a7c98e8ee8cdd3199db68092b48868847d4743a471b26afc2ff878bafaa829ed43ee405f9aff58ae13fce53b898f7c2e3c30cb80af8eb111682c3c13f686dbb@18.198.130.54:30303",
    "enode://2c4307831914c237801993eac4f9596d8b2f78e1e76830419b64cb23f0933e52cb1e2bb3009cb4af76454bb5bc296135b36869fd6c13e2c2e536a0780e60fe82@3.64.242.196:30303",
    "enode://074f68e1a7df5b0859314ff721d55b59d9690e93249c941660609a29b302f02864df4f93ee48884f7ede57dc7f7646379d017a43c9745e34baff049749896b50@3.126.169.151:30303",
    "enode://d239697375d7586c7ea1de790401c310b0b1d389326849fa3b7c7005833c7a6b9020e49dfb3b61abfa39135237ffc4ff219cb84ca7653069e8548497527aa432@107.22.4.120:30303",
    "enode://d5852bf415d89b756faa809f4ff3f8beb661dc7d60cfb4a5542f9a5fcdf41e1ed0708a210db64b8c7ca32426e04ef0a50da58974124fdf562a8510314d11e28c@3.26.206.142:30303",
    "enode://01d372392bb22dd8a91f8b10b6bbb8d80d2dbe98d695801e0df9e4bd4825781df84bba88361f24d1b6580a61313f64e6cec82e8d842ad5f1b3d7cf8d6d132da7@15.152.45.82:30303",
    "enode://aee88e803b8e54925081957965b2527961cd90f4d6d14664884580b429da44729678a1258a8b49a42d1582c9c7c5ded05733622f7ab442ad9c6f655545a5ecdd@54.207.220.169:30303",
    "enode://fb14d72321ee823fcf21e163091849ee42e0f6ac0cddc737d79e324b0a734c4fc51823ef0a96b749c954483c25e8d2e534d1d5fc2619ea22d58671aff96f5188@65.109.103.148:30303",
    "enode://40f40acd78004650cce57aa302de9acbf54becf91b609da93596a18979bb203ba79fcbee5c2e637407b91be23ce72f0cc13dfa38d13e657005ce842eafb6b172@65.109.103.149:30303",
    "enode://9e50857aa48a7a31bc7b46957e8ced0ef69a7165d3199bea924cb6d02b81f1f35bd8e29d21a54f4a331316bf09bb92716772ea76d3ef75ce027699eccfa14fad@141.94.97.22:30303",
    "enode://96dc133ce3aeb5d9430f1dce1d77a36418c8789b443ae0445f06f73c6b363f5b35c019086700a098c3e6e54974d64f37e97d72a5c711d1eae34dc06e3e00eed5@141.94.97.74:30303",
    "enode://516cbfbe9bbf26b6395ed68b24e383401fc33e7fe96b9d235ebca86c9f812fde8d33a7dbebc0fb5595459d2c5cc6381595d96507af89e6b48b5bdd0ebf8af0c0@141.94.97.84:30303",
    "enode://fc86a93545c56322dd861180b76632b9baeb65af8f304269b489b4623ae060847569c3c3c10c4b39baf221a2cdefea66efabce061a542cdcda374cbba45aa3d4@51.68.39.206:30303",
    "enode://0e6dd3815a627893515465130c1e95aa73b18fe2f723b2467f3abf94df9be036f27595f301b5e78750ad128e59265f980c92033ae903330c0460c40ae088c04a@35.210.37.245:30303",
    "enode://b72d6233d50bef7b31c09f3ea39459257520178f985a872bbaa4e371ed619455b7671053ffe985af1b5fb3270606e2a49e4e67084debd75e6c9b93e227c5b01c@35.210.156.59:30303",
];

const CHIADO_NODES: &[&str] = &[
    "enode://3c9849b809dc34c914fcdf507ff59931942bdf5cd11510782a7d5695eacd622a281ae0b46aa53b8b893e1308f94867001d0fb0b52c854f96e7ecf43490f5b7bb@139.144.26.89:30303",
    "enode://1556022f95f2910ed795b80df68466284b5a7de112cb00d5f4843b486392e7e790c9d27cf358bf1e3ceff3089df36dcadae593eda9730565a7221e40a96b8cd4@139.144.26.115:30303",
    "enode://b39e929805542fb141ca6946931d06fdbbbd9ea8202eb1e72d7e7484877658b5baf0d8b0a6eb86a7f34b6f5c4b15b080c807c851692baef96c6a5aeca2cbf29a@139.144.26.101:30303",
    "enode://5f7074a38a84e7ed7cbb207b9b1a54b4c537c5d06ebc955c892528f95927eb63a3373e344302cb1bcc2242451899c276a21e360a4347674cb97e0b9c251c2704@139.144.26.85:30303",
    "enode://9ff64d021a83c72d68e7a3cefac5ea0661071cfdeed065782418ff2b0fccaace1072644118ed9fe6a304f451a2e75e2d4c69b502b3486ce16b4c50afc347cbff@170.187.154.239:30303",
    "enode://4504f03b4773251188e80d2c36186de4c2dd0e1e83aadaa1164cdae2ebc510d47a3dba6c80972ea18a71177ab3aa9883e081f5a350e8979cb7127e63bb6b81ea@139.144.173.54:30303",
    "enode://712144ac396fd2298b3e2559e2930d7f3a36fded3addd66955224958f1845634067717ab9522757ed2948f480fc52add5676487c8378e9011a7e2c0ac2f36cc3@3.71.132.231:30303",
    "enode://595160631241ea41b187b85716f9f9572a266daa940d74edbe3b83477264ce284d69208e61cf50e91641b1b4f9a03fa8e60eb73d435a84cf4616b1c969bc2512@3.69.35.13:30303",
    "enode://5abc2f73f81ea6b94f1e1b1e376731fc662ecd7863c4c7bc83ec307042542a64feab5af7985d52b3b1432acf3cb82460b327d0b6b70cb732afb1e5a16d6b1e58@35.206.174.92:30303",
    "enode://f7e62226a64a2ccc0ada8b032b33c4389464562f87135a3e0d5bdb814fab717d58db5d142c453b071d08b4e0ffd9c5aff4a6d4441c2041401634f10d7962f885@35.210.126.23:30303",
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

fn genesis_hash(chain_id: u64, chainspec_genesis_hash: B256) -> B256 {
    match Chain::from_chain_id(chain_id) {
        Some(Chain::Gnosis) => {
            b256!("4f1dd23188aab3a76b463e4af801b52b1248ef073c648cbdc4c9333d3da79756")
        }
        Some(Chain::Chiado) => {
            b256!("ada44fd8d2ecab8b08f256af07ad3e777f17fb434f8f8e678b312f576212ba9a")
        }
        None => chainspec_genesis_hash,
    }
}

/// Chain spec builder for gnosis chain.
#[derive(Debug, Default, From)]
pub struct GnosisChainSpecBuilder {
    /// [`ChainSpecBuilder`]
    _inner: ChainSpecBuilder,
}

/// Gnosis chain spec type.
#[derive(Debug, Clone, Default, Deref, Into, Constructor, PartialEq, Eq)]
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

    fn blob_params_at_timestamp(&self, timestamp: u64) -> Option<BlobParams> {
        self.inner.blob_params_at_timestamp(timestamp)
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

    fn bootnodes(&self) -> Option<Vec<NodeRecord>> {
        if let Some(chain) = Chain::from_chain_id(self.chain_id()) {
            match chain {
                Chain::Gnosis => Some(parse_nodes(GNOSIS_NODES)),
                Chain::Chiado => Some(parse_nodes(CHIADO_NODES)),
            }
        } else {
            None
        }
    }

    fn final_paris_total_difficulty(&self) -> Option<U256> {
        self.inner.final_paris_total_difficulty()
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
        let mut forkhash = ForkHash::from(genesis_hash(self.chain_id(), self.genesis_hash()));
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
                if head.number >= block {
                    // skip duplicated hardforks: hardforks enabled at genesis block
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
            // ensure we only get timestamp forks activated __after__ the genesis block
            cond.as_timestamp()
                .filter(|time| time > &self.genesis.timestamp)
        }) {
            if head.timestamp >= timestamp {
                // skip duplicated hardfork activated at the same timestamp
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
            genesis_hash(self.chain_id(), self.genesis_hash()),
            self.genesis_timestamp(),
            forks,
        )
    }
}

impl EthereumHardforks for GnosisChainSpec {
    fn ethereum_fork_activation(&self, fork: EthereumHardfork) -> ForkCondition {
        self.fork(fork)
    }
}

impl EthExecutorSpec for GnosisChainSpec {
    fn deposit_contract_address(&self) -> Option<Address> {
        self.deposit_contract
            .map(|deposit_contract| deposit_contract.address)
    }
}

impl From<Genesis> for GnosisChainSpec {
    fn from(genesis: Genesis) -> Self {
        let chain_id = genesis.config.chain_id;

        // Block-based hardforks
        let mainnet_hardfork_opts: [(Box<dyn Hardfork>, Option<u64>); 12] = [
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
            (
                EthereumHardfork::Berlin.boxed(),
                genesis.config.berlin_block,
            ),
            (
                EthereumHardfork::London.boxed(),
                genesis.config.london_block,
            ),
        ];

        let chiado_hardfork_opts: [(Box<dyn Hardfork>, Option<u64>); 9] = [
            (
                EthereumHardfork::Homestead.boxed(),
                genesis.config.homestead_block,
            ),
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
            (
                EthereumHardfork::Petersburg.boxed(),
                genesis.config.petersburg_block,
            ),
            (
                EthereumHardfork::Istanbul.boxed(),
                genesis.config.istanbul_block,
            ),
            (
                EthereumHardfork::Berlin.boxed(),
                genesis.config.berlin_block,
            ),
            (
                EthereumHardfork::London.boxed(),
                genesis.config.london_block,
            ),
        ];

        let mut hardforks = match Chain::from_chain_id(chain_id) {
            Some(Chain::Gnosis) => mainnet_hardfork_opts
                .into_iter()
                .filter_map(|(hardfork, opt)| {
                    opt.map(|block| (hardfork, ForkCondition::Block(block)))
                })
                .collect::<Vec<_>>(),
            _ => chiado_hardfork_opts
                .into_iter()
                .filter_map(|(hardfork, opt)| {
                    opt.map(|block| (hardfork, ForkCondition::Block(block)))
                })
                .collect::<Vec<_>>(),
        };

        // Paris
        let paris_block_and_final_difficulty =
            if let Some(ttd) = genesis.config.terminal_total_difficulty {
                hardforks.push((
                    EthereumHardfork::Paris.boxed(),
                    ForkCondition::TTD {
                        // NOTE: this will not work properly if the merge is not activated at
                        // genesis, and there is no merge netsplit block
                        activation_block_number: genesis
                            .config
                            .merge_netsplit_block
                            .unwrap_or_default(),
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
                        "0x649bbc62d0e31342afea4e5cd82d4049e7e1ee912fc0889aa790803be39038c5"
                    ),
                });

        let hardforks = ChainHardforks::new(ordered_hardforks);

        Self {
            inner: ChainSpec {
                chain: genesis.config.chain_id.into(),
                genesis_header: SealedHeader::new_unhashed(make_genesis_header(
                    &genesis, &hardforks,
                )),
                genesis,
                hardforks,
                paris_block_and_final_difficulty,
                deposit_contract,
                blob_params: GNOSIS_BLOB_SCHEDULE,
                base_fee_params: BaseFeeParamsKind::Constant(BaseFeeParams::ethereum()),
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

    const SUPPORTED_CHAINS: &'static [&'static str] = &["dev", "chiado", "gnosis"];

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
        // TODO: allow for hardcoded built-in chains. using Genesis::default() because fallback needed for clap
        "dev" => Arc::new(GnosisChainSpec::from(Genesis::default())),
        "chiado" => Arc::new(GnosisChainSpec::from(CHIADO_GENESIS.clone())),
        "gnosis" => Arc::new(GnosisChainSpec::from(GNOSIS_GENESIS.clone())),
        _ => Arc::new(parse_genesis(s)?.into()),
    })
}
