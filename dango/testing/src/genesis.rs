use {
    crate::{
        BridgeOp, TestOption,
        constants::{
            DEFAULT_GAS_LIMIT, MOCK_BLOCK_TIME, MOCK_CHAIN_ID, MOCK_GENESIS_TIMESTAMP,
            MOCK_HYPERLANE_LOCAL_DOMAIN, MOCK_HYPERLANE_VALIDATOR_ADDRESSES, mock_arbitrum,
            mock_ethereum, owner, user1, user2, user3, user4, user5, user6, user7, user8, user9,
        },
    },
    dango_genesis::{
        AccountOption, BankOption, GatewayOption, GenesisOption, GenesisUser, GrugOption,
        HyperlaneOption, OracleOption, PerpsOption, VestingOption,
    },
    dango_hyperlane_types::isms::multisig::ValidatorSet,
    dango_math::{NumberConst, Udec128, Uint128},
    dango_order_book::{Dimensionless, Quantity, UsdPrice},
    dango_primitives::{
        Addressable, BlockInfo, Bounded, Coin, Coins, Denom, Duration, GENESIS_BLOCK_HASH,
        GENESIS_BLOCK_HEIGHT, HashExt, LengthBounded, Op, Timestamp, btree_map, btree_set,
    },
    dango_types::{
        account_factory::NewUserSalt,
        auth::Key,
        bank::Metadata,
        constants::{PYTH_PRICE_SOURCES, dango, eth, usdc},
        gateway::{Origin, Remote, WithdrawalFee},
        perps::{self, PairParam},
    },
};

/// Describing a data that has a preset value for testing purposes.
pub trait Preset {
    fn preset_test() -> Self;
}

impl Preset for TestOption {
    fn preset_test() -> Self {
        TestOption {
            chain_id: MOCK_CHAIN_ID.to_string(),
            block_time: MOCK_BLOCK_TIME,
            default_gas_limit: DEFAULT_GAS_LIMIT,
            genesis_block: BlockInfo {
                hash: GENESIS_BLOCK_HASH,
                height: GENESIS_BLOCK_HEIGHT,
                timestamp: MOCK_GENESIS_TIMESTAMP,
            },
            mocked_clickhouse: false,
            // By default, give the owner and each user 100k USDC from Ethereum.
            bridge_ops: |accounts| {
                vec![
                    BridgeOp {
                        remote: Remote::Warp {
                            domain: mock_ethereum::DOMAIN,
                            contract: mock_ethereum::USDC_WARP,
                        },
                        amount: Uint128::new(100_000_000_000),
                        recipient: accounts.owner.address(),
                    },
                    BridgeOp {
                        remote: Remote::Warp {
                            domain: mock_ethereum::DOMAIN,
                            contract: mock_ethereum::ETH_WARP,
                        },
                        amount: Uint128::new(100_000_000_000),
                        recipient: accounts.owner.address(),
                    },
                    BridgeOp {
                        remote: Remote::Warp {
                            domain: mock_ethereum::DOMAIN,
                            contract: mock_ethereum::USDC_WARP,
                        },
                        amount: Uint128::new(100_000_000_000),
                        recipient: accounts.user1.address(),
                    },
                    BridgeOp {
                        remote: Remote::Warp {
                            domain: mock_ethereum::DOMAIN,
                            contract: mock_ethereum::ETH_WARP,
                        },
                        amount: Uint128::new(20_000_000_000_000_000_000), // 20 ETH
                        recipient: accounts.user1.address(),
                    },
                    BridgeOp {
                        remote: Remote::Warp {
                            domain: mock_ethereum::DOMAIN,
                            contract: mock_ethereum::USDC_WARP,
                        },
                        amount: Uint128::new(100_000_000_000),
                        recipient: accounts.user2.address(),
                    },
                    BridgeOp {
                        remote: Remote::Warp {
                            domain: mock_ethereum::DOMAIN,
                            contract: mock_ethereum::ETH_WARP,
                        },
                        amount: Uint128::new(100_000_000_000),
                        recipient: accounts.user2.address(),
                    },
                    BridgeOp {
                        remote: Remote::Warp {
                            domain: mock_ethereum::DOMAIN,
                            contract: mock_ethereum::USDC_WARP,
                        },
                        amount: Uint128::new(100_000_000_000),
                        recipient: accounts.user3.address(),
                    },
                    BridgeOp {
                        remote: Remote::Warp {
                            domain: mock_ethereum::DOMAIN,
                            contract: mock_ethereum::USDC_WARP,
                        },
                        amount: Uint128::new(100_000_000_000),
                        recipient: accounts.user4.address(),
                    },
                    BridgeOp {
                        remote: Remote::Warp {
                            domain: mock_ethereum::DOMAIN,
                            contract: mock_ethereum::USDC_WARP,
                        },
                        amount: Uint128::new(100_000_000_000),
                        recipient: accounts.user5.address(),
                    },
                    BridgeOp {
                        remote: Remote::Warp {
                            domain: mock_ethereum::DOMAIN,
                            contract: mock_ethereum::USDC_WARP,
                        },
                        amount: Uint128::new(100_000_000_000),
                        recipient: accounts.user6.address(),
                    },
                    BridgeOp {
                        remote: Remote::Warp {
                            domain: mock_ethereum::DOMAIN,
                            contract: mock_ethereum::USDC_WARP,
                        },
                        amount: Uint128::new(100_000_000_000),
                        recipient: accounts.user7.address(),
                    },
                    BridgeOp {
                        remote: Remote::Warp {
                            domain: mock_ethereum::DOMAIN,
                            contract: mock_ethereum::USDC_WARP,
                        },
                        amount: Uint128::new(100_000_000_000),
                        recipient: accounts.user8.address(),
                    },
                    BridgeOp {
                        remote: Remote::Warp {
                            domain: mock_ethereum::DOMAIN,
                            contract: mock_ethereum::USDC_WARP,
                        },
                        amount: Uint128::new(100_000_000_000),
                        recipient: accounts.user9.address(),
                    },
                ]
            },
        }
    }
}

impl Preset for GenesisOption {
    fn preset_test() -> Self {
        GenesisOption {
            grug: Preset::preset_test(),
            account: Preset::preset_test(),
            bank: Preset::preset_test(),
            gateway: Preset::preset_test(),
            hyperlane: Preset::preset_test(),
            oracle: Preset::preset_test(),
            perps: Preset::preset_test(),
            vesting: Preset::preset_test(),
        }
    }
}

impl Preset for GrugOption {
    fn preset_test() -> Self {
        GrugOption {
            owner_index: 0,
            gas_token: usdc::DENOM.clone(),
            gas_fee_rate: Udec128::ZERO, // Use zero gas price for testing.
            max_orphan_age: Duration::from_weeks(1),
        }
    }
}

impl Preset for AccountOption {
    fn preset_test() -> Self {
        AccountOption {
            genesis_users: vec![
                GenesisUser {
                    salt: NewUserSalt {
                        key: Key::Secp256k1(owner::PUBLIC_KEY.into()),
                        key_hash: owner::PUBLIC_KEY.sha2_256(),
                        seed: 0,
                    },
                    dango_balance: Uint128::new(100_000_000_000_000),
                },
                GenesisUser {
                    salt: NewUserSalt {
                        key: Key::Secp256k1(user1::PUBLIC_KEY.into()),
                        key_hash: user1::PUBLIC_KEY.sha2_256(),
                        seed: 1,
                    },
                    dango_balance: Uint128::new(1_000_000_000_000_000_000),
                },
                GenesisUser {
                    salt: NewUserSalt {
                        key: Key::Secp256k1(user2::PUBLIC_KEY.into()),
                        key_hash: user2::PUBLIC_KEY.sha2_256(),
                        seed: 2,
                    },
                    dango_balance: Uint128::new(100_000_000_000_000),
                },
                GenesisUser {
                    salt: NewUserSalt {
                        key: Key::Secp256k1(user3::PUBLIC_KEY.into()),
                        key_hash: user3::PUBLIC_KEY.sha2_256(),
                        seed: 3,
                    },
                    dango_balance: Uint128::new(100_000_000_000_000),
                },
                GenesisUser {
                    salt: NewUserSalt {
                        key: Key::Secp256k1(user4::PUBLIC_KEY.into()),
                        key_hash: user4::PUBLIC_KEY.sha2_256(),
                        seed: 4,
                    },
                    dango_balance: Uint128::new(100_000_000_000_000),
                },
                GenesisUser {
                    salt: NewUserSalt {
                        key: Key::Secp256k1(user5::PUBLIC_KEY.into()),
                        key_hash: user5::PUBLIC_KEY.sha2_256(),
                        seed: 5,
                    },
                    dango_balance: Uint128::new(100_000_000_000_000),
                },
                GenesisUser {
                    salt: NewUserSalt {
                        key: Key::Secp256k1(user6::PUBLIC_KEY.into()),
                        key_hash: user6::PUBLIC_KEY.sha2_256(),
                        seed: 6,
                    },
                    dango_balance: Uint128::new(100_000_000_000_000),
                },
                GenesisUser {
                    salt: NewUserSalt {
                        key: Key::Secp256k1(user7::PUBLIC_KEY.into()),
                        key_hash: user7::PUBLIC_KEY.sha2_256(),
                        seed: 7,
                    },
                    dango_balance: Uint128::new(100_000_000_000_000),
                },
                GenesisUser {
                    salt: NewUserSalt {
                        key: Key::Secp256k1(user8::PUBLIC_KEY.into()),
                        key_hash: user8::PUBLIC_KEY.sha2_256(),
                        seed: 8,
                    },
                    dango_balance: Uint128::new(100_000_000_000_000),
                },
                GenesisUser {
                    salt: NewUserSalt {
                        key: Key::Secp256k1(user9::PUBLIC_KEY.into()),
                        key_hash: user9::PUBLIC_KEY.sha2_256(),
                        seed: 9,
                    },
                    dango_balance: Uint128::new(100_000_000_000_000),
                },
            ],
            minimum_deposit: Coins::new(),
        }
    }
}

impl Preset for BankOption {
    fn preset_test() -> Self {
        BankOption {
            metadatas: btree_map! {
                dango::DENOM.clone() => Metadata {
                    name: LengthBounded::new_unchecked("Dango".to_string()),
                    symbol: LengthBounded::new_unchecked("DGX".to_string()),
                    decimals: dango::DECIMAL,
                    description: Some(LengthBounded::new_unchecked("Native token of Dango".to_string())),
                },
                eth::DENOM.clone() => Metadata {
                    name: LengthBounded::new_unchecked("Ether".to_string()),
                    symbol: LengthBounded::new_unchecked("ETH".to_string()),
                    decimals: eth::DECIMAL,
                    description: None,
                },
                usdc::DENOM.clone() => Metadata {
                    name: LengthBounded::new_unchecked("USD Coin".to_string()),
                    symbol: LengthBounded::new_unchecked("USDC".to_string()),
                    decimals: usdc::DECIMAL,
                    description: None,
                },
            },
        }
    }
}

impl Preset for GatewayOption {
    fn preset_test() -> Self {
        GatewayOption {
            warp_routes: btree_set! {
                (Origin::Remote(usdc::SUBDENOM.clone()), Remote::Warp {
                    domain: mock_arbitrum::DOMAIN,
                    contract: mock_arbitrum::USDC_WARP,
                }),
                (Origin::Remote(usdc::SUBDENOM.clone()), Remote::Warp {
                    domain: mock_ethereum::DOMAIN,
                    contract: mock_ethereum::USDC_WARP,
                }),
                (Origin::Remote(eth::SUBDENOM.clone()), Remote::Warp {
                    domain: mock_arbitrum::DOMAIN,
                    contract: mock_arbitrum::ETH_WARP,
                }),
                (Origin::Remote(eth::SUBDENOM.clone()), Remote::Warp {
                    domain: mock_ethereum::DOMAIN,
                    contract: mock_ethereum::ETH_WARP,
                }),
            },
            withdrawal_fees: vec![
                WithdrawalFee {
                    denom: usdc::DENOM.clone(),
                    remote: Remote::Warp {
                        domain: mock_arbitrum::DOMAIN,
                        contract: mock_arbitrum::USDC_WARP,
                    },
                    fee: Op::Insert(Uint128::new(10_000)),
                },
                WithdrawalFee {
                    denom: usdc::DENOM.clone(),
                    remote: Remote::Warp {
                        domain: mock_ethereum::DOMAIN,
                        contract: mock_ethereum::USDC_WARP,
                    },
                    fee: Op::Insert(Uint128::new(1_000_000)),
                },
                WithdrawalFee {
                    denom: eth::DENOM.clone(),
                    remote: Remote::Warp {
                        domain: mock_arbitrum::DOMAIN,
                        contract: mock_arbitrum::ETH_WARP,
                    },
                    fee: Op::Insert(Uint128::new(50_000_000_000_000)),
                },
                WithdrawalFee {
                    denom: eth::DENOM.clone(),
                    remote: Remote::Warp {
                        domain: mock_ethereum::DOMAIN,
                        contract: mock_ethereum::ETH_WARP,
                    },
                    fee: Op::Insert(Uint128::new(500_000_000_000_000)),
                },
            ],
            rate_limits: btree_map! {
                usdc::DENOM.clone() => Bounded::new_unchecked(Udec128::new_percent(10)),
                eth::DENOM.clone()  => Bounded::new_unchecked(Udec128::new_percent(10)),
            },
            rate_limit_refresh_period: Duration::from_days(1),
            // No guardian in the test genesis; the owner responds to
            // withdrawal requests. Tests that exercise the guardian flow set
            // one at runtime via `SetGuardian`.
            guardian: None,
        }
    }
}

impl Preset for HyperlaneOption {
    fn preset_test() -> Self {
        // We use the same mock validator set for all remote domains.
        let mock_validator_set = ValidatorSet {
            threshold: 2,
            validators: MOCK_HYPERLANE_VALIDATOR_ADDRESSES.into_iter().collect(),
        };

        HyperlaneOption {
            local_domain: MOCK_HYPERLANE_LOCAL_DOMAIN,
            ism_validator_sets: btree_map! {
                mock_arbitrum::DOMAIN => mock_validator_set.clone(),
                mock_ethereum::DOMAIN => mock_validator_set,
            },
            va_announce_fee_per_byte: Coin {
                denom: usdc::DENOM.clone(),
                amount: Uint128::new(100),
            },
        }
    }
}

impl Preset for OracleOption {
    fn preset_test() -> Self {
        let pubkey = crate::pyth::mock_pyth_trusted_signer();

        OracleOption {
            pyth_price_sources: PYTH_PRICE_SOURCES.clone(),
            pyth_trusted_signers: btree_map! { pubkey => Timestamp::from_nanos(u128::MAX) },
        }
    }
}

impl Preset for PerpsOption {
    fn preset_test() -> Self {
        let pair_id: Denom = "perp/ethusd".parse().unwrap();
        PerpsOption {
            param: perps::Param {
                taker_fee_rates: perps::RateSchedule {
                    base: Dimensionless::new_permille(1), // 0.1%
                    ..Default::default()
                },
                protocol_fee_rate: Dimensionless::ZERO,
                liquidation_fee_rate: Dimensionless::new_permille(10), // 1%
                vault_cooldown_period: Duration::from_days(1),
                max_unlocks: 10,
                max_open_orders: 100,
                funding_period: Duration::from_hours(1),
                referral_active: true,
                max_action_batch_size: 5,
                ..Default::default()
            },
            pair_params: btree_map! {
                pair_id => PairParam {
                    initial_margin_ratio: Dimensionless::new_permille(100), // 10%
                    maintenance_margin_ratio: Dimensionless::new_permille(50), // 5%
                    tick_size: UsdPrice::new_int(1),
                    max_abs_oi: Quantity::new_int(1_000_000),
                    ..PairParam::new_mock()
                },
            },
        }
    }
}

impl Preset for VestingOption {
    fn preset_test() -> Self {
        VestingOption {
            unlocking_cliff: Duration::from_weeks(4 * 9), // ~9 months
            unlocking_period: Duration::from_weeks(4 * 27), // ~27 months
        }
    }
}
