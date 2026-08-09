/// The default gas costs.
///
/// For now, we make this a constant. In the future we can consider making this
/// an on-chain parameter configurable by governance.
pub const GAS_COSTS: GasCosts = GasCosts {
    // Storage.
    //
    // For storage, we take the values from Cosmos SDK:
    // https://github.com/cosmos/cosmos-sdk/blob/v0.50.7/store/types/gas.go#L232-L242
    //
    // Following the conversion:
    // - 1 Cosmos SDK gas = 100 CosmWasm gas
    // - 170 CosmWasm gas = 1 Wasmer point
    // - 1 Wasmer point = 1 Dango gas
    // This means: 1 Cosmos SDK gas = 0.588 Dango gas
    db_read: LinearGasCost::new(588, 2),
    db_scan: 588,
    db_next: 18,
    db_write: LinearGasCost::new(1176, 18),
    db_remove: 588,
    // Verifiers
    //
    // For batch verification, there's a flat setup cost, and a cost per signature.
    secp256r1_verify: 1_880_000,
    secp256k1_verify: 770_000,
    secp256k1_pubkey_recover: 1_580_000,
    // Hashers.
    //
    // For hashers, `per_item` means per byte.
    // The truncated versions have the same cost as the untruncated counterparts.
    sha2_256: LinearGasCost::new(0, 27),
    keccak256: LinearGasCost::new(0, 15),
};

pub struct GasCosts {
    // Storage
    pub db_read: LinearGasCost,
    pub db_scan: u64,
    pub db_next: u64,
    pub db_write: LinearGasCost,
    pub db_remove: u64,
    // Signature verifiers
    pub secp256r1_verify: u64,
    pub secp256k1_verify: u64,
    pub secp256k1_pubkey_recover: u64,
    // Hashers
    pub sha2_256: LinearGasCost,
    pub keccak256: LinearGasCost,
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
