use {
    dango_hyperlane_types::{isms::multisig::ValidatorSet, mailbox::Domain},
    dango_math::{Udec128, Uint128},
    dango_order_book::PairId,
    dango_primitives::{Addr, Binary, Coin, Coins, Denom, Duration, Hash256, HashExt, Timestamp},
    dango_types::{
        account_factory::{NewUserSalt, UserIndex},
        bank,
        config::Hyperlane,
        gateway::{Origin, RateLimit, Remote, WithdrawalFee},
        oracle::PriceConfig,
        perps::{self, PairParam},
    },
    std::collections::{BTreeMap, BTreeSet, HashSet},
};

#[dango_primitives::derive(Serde)]
pub struct Contracts {
    pub account_factory: Addr,
    pub bank: Addr,
    pub gateway: Addr,
    pub hyperlane: Hyperlane<Addr>,
    pub oracle: Addr,
    pub perps: Addr,
    pub vesting: Addr,
    pub warp: Addr,
}

#[derive(Clone, Copy)]
pub struct Codes<T> {
    pub account: T,
    pub account_factory: T,
    pub bank: T,
    pub gateway: T,
    pub hyperlane: Hyperlane<T>,
    pub oracle: T,
    pub perps: T,
    pub vesting: T,
    pub warp: T,
}

impl<T> Codes<T>
where
    T: Clone + Into<Binary>,
{
    pub fn all_code_hashes(&self) -> HashSet<Hash256> {
        [
            &self.account,
            &self.account_factory,
            &self.bank,
            &self.gateway,
            &self.hyperlane.ism,
            &self.hyperlane.mailbox,
            &self.hyperlane.va,
            &self.oracle,
            &self.perps,
            &self.vesting,
            &self.warp,
        ]
        .into_iter()
        .map(|code| {
            let binary: Binary = code.clone().into();
            binary.sha2_256()
        })
        .collect()
    }
}

pub struct GenesisUser {
    pub salt: NewUserSalt,
    pub dango_balance: Uint128,
}

pub struct GenesisOption {
    pub grug: GrugOption,
    pub account: AccountOption,
    pub bank: BankOption,
    pub gateway: GatewayOption,
    pub hyperlane: HyperlaneOption,
    pub oracle: OracleOption,
    pub perps: PerpsOption,
    pub vesting: VestingOption,
}

pub struct GrugOption {
    /// A user index whose genesis account is to be appointed as the owner.
    /// We expect to transfer ownership to a multisig account afterwards.
    pub owner_index: UserIndex,
    /// The token in which transaction fees are denominated.
    pub gas_token: Denom,
    /// The amount of `gas_token` charged per unit of gas.
    pub gas_fee_rate: Udec128,
    /// The maximum age a contract bytecode can remain orphaned (not used by any
    /// contract).
    /// Once this time is elapsed, the code is deleted and must be uploaded again.
    pub max_orphan_age: Duration,
}

pub struct AccountOption {
    /// Initial users and their balances.
    /// For each genesis user will be created a single-signature account.
    pub genesis_users: Vec<GenesisUser>,
    /// The minimum deposit required to onboard a user.
    pub minimum_deposit: Coins,
}

pub struct BankOption {
    /// Metadata of tokens.
    pub metadatas: BTreeMap<Denom, bank::Metadata>,
}

pub struct GatewayOption {
    // Note: these are only the Hyperlane Warp routes. No need to specify the
    // bitcoin bridge route here.
    pub warp_routes: BTreeSet<(Origin, Remote)>,
    pub rate_limits: BTreeMap<Denom, RateLimit>,
    pub rate_limit_refresh_period: Duration,
    pub withdrawal_fees: Vec<WithdrawalFee>,
    /// The whitelisted address that responds to withdrawal requests. If
    /// unset, only the chain owner can respond.
    pub guardian: Option<Addr>,
}

pub struct HyperlaneOption {
    /// Hyperlane domain ID of the local domain.
    pub local_domain: Domain,
    /// Hyperlane validator sets for remote domains.
    pub ism_validator_sets: BTreeMap<Domain, ValidatorSet>,
    /// Hyperlane validator announce fee rate.
    pub va_announce_fee_per_byte: Coin,
}

pub struct OracleOption {
    /// Oracle price sources.
    pub pyth_price_sources: BTreeMap<Denom, PriceConfig>,
    /// Pyth Lazer trusted signers: public key and expiration timestamp.
    pub pyth_trusted_signers: BTreeMap<Binary, Timestamp>,
}

pub struct PerpsOption {
    /// Global parameters for the perpetuals contract.
    pub param: perps::Param,
    /// Per-pair parameters, keyed by the pair ID (e.g. "perp/ethusd").
    pub pair_params: BTreeMap<PairId, PairParam>,
}

pub struct VestingOption {
    /// Cliff for Dango token unlocking.
    pub unlocking_cliff: Duration,
    /// Period for Dango token unlocking.
    pub unlocking_period: Duration,
}
