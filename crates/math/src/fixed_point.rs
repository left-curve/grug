use {
    crate::{Dec128, Dec256, Int128, Int256, Udec128, Udec256, Uint, Uint128, Uint256},
    bnum::types::{I256, U256},
};

/// Describes a [fixed-point decimal](https://en.wikipedia.org/wiki/Fixed-point_arithmetic)
/// number.
pub trait FixedPoint<U> {
    /// Ratio between the inner integer value and the decimal value it
    /// represents.
    const DECIMAL_FRACTION: Uint<U>;

    /// Number of decimal digits to be interpreted as decimal places.
    const DECIMAL_PLACES: u32;
}

impl FixedPoint<u128> for Udec128 {
    const DECIMAL_FRACTION: Uint128 = Uint128::new(1_000_000_000_000_000_000);
    const DECIMAL_PLACES: u32 = 18;
}

impl FixedPoint<U256> for Udec256 {
    const DECIMAL_FRACTION: Uint256 = Uint256::new_from_u128(1_000_000_000_000_000_000);
    const DECIMAL_PLACES: u32 = 18;
}

impl FixedPoint<i128> for Dec128 {
    const DECIMAL_FRACTION: Int128 = Int128::new(1_000_000_000_000_000_000);
    const DECIMAL_PLACES: u32 = 18;
}

impl FixedPoint<I256> for Dec256 {
    const DECIMAL_FRACTION: Int256 = Int256::new_from_i128(1_000_000_000_000_000_000);
    const DECIMAL_PLACES: u32 = 18;
}
