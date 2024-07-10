/// The default gas costs.
///
/// TODO: We can put this in the chain's `Config`, such that this can be changed
/// via a governance proposal, without having to fork the chain.
pub const GAS_COSTS: GasCosts = GasCosts {
    secp256r1_verify_cost: 1_880_000,
    secp256k1_verify_cost: 770_000,
    secp256k1_pubkey_recover: 1_580_000,
    ed25519_verify_cost: 410_000,
    // For batch verification, there's a flat setup cost, and a cost per signature.
    ed25519_batch_verify_cost: LinearGasCost::new(1_340_000, 188_000),
    // For hashers, `per_item` means per byte.
    // The truncated versions have the same cost as the untruncated counterparts.
    sha2_256: LinearGasCost::new(0, 27_265),
    sha2_512: LinearGasCost::new(0, 16_814),
    sha2_512_truncated: LinearGasCost::new(0, 16_814),
    sha3_256: LinearGasCost::new(0, 15_326),
    sha3_512: LinearGasCost::new(0, 28_910),
    sha3_512_truncated: LinearGasCost::new(0, 28_910),
    keccak256: LinearGasCost::new(0, 15_265),
    blake2s_256: LinearGasCost::new(0, 15_244),
    blake2b_512: LinearGasCost::new(0, 9_114),
    blake3: LinearGasCost::new(0, 5_195),
};

pub struct GasCosts {
    pub secp256r1_verify_cost: u64,
    pub secp256k1_verify_cost: u64,
    pub secp256k1_pubkey_recover: u64,
    pub ed25519_verify_cost: u64,
    pub ed25519_batch_verify_cost: LinearGasCost,
    pub sha2_256: LinearGasCost,
    pub sha2_512: LinearGasCost,
    pub sha2_512_truncated: LinearGasCost,
    pub sha3_256: LinearGasCost,
    pub sha3_512: LinearGasCost,
    pub sha3_512_truncated: LinearGasCost,
    pub keccak256: LinearGasCost,
    pub blake2s_256: LinearGasCost,
    pub blake2b_512: LinearGasCost,
    pub blake3: LinearGasCost,
}

pub struct LinearGasCost {
    /// The flat part of the cost, charged once per batch.
    base: u64,
    /// The cost per item, on top of the flat part.
    per_item: u64,
}

impl LinearGasCost {
    pub const fn new(base: u64, per_item: u64) -> Self {
        Self { base, per_item }
    }

    pub fn cost(&self, items: usize) -> u64 {
        self.base + self.per_item * items as u64
    }
}