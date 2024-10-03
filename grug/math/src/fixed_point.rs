use {
    crate::{Dec128, Dec256, Int, Int128, Int256, Udec128, Udec256, Uint128, Uint256},
    bnum::types::{I256, U256},
};

/// Describes a [fixed-point decimal](https://en.wikipedia.org/wiki/Fixed-point_arithmetic)
/// number.
pub trait FixedPoint<U> {
    /// Ratio between the inner integer value and the decimal value it
    /// represents.
    const DECIMAL_FRACTION: Int<U>;

    /// Number of decimal digits to be interpreted as decimal places.
    const DECIMAL_PLACES: u32;
}

impl FixedPoint<u128> for Udec128 {
    const DECIMAL_FRACTION: Uint128 = Uint128::new(10_u128.pow(Self::DECIMAL_PLACES));
    const DECIMAL_PLACES: u32 = 18;
}

impl FixedPoint<U256> for Udec256 {
    const DECIMAL_FRACTION: Uint256 = Uint256::new_from_u128(10_u128.pow(Self::DECIMAL_PLACES));
    const DECIMAL_PLACES: u32 = 18;
}

impl FixedPoint<i128> for Dec128 {
    const DECIMAL_FRACTION: Int128 = Int128::new(10_i128.pow(Self::DECIMAL_PLACES));
    const DECIMAL_PLACES: u32 = 18;
}

impl FixedPoint<I256> for Dec256 {
    const DECIMAL_FRACTION: Int256 = Int256::new_from_i128(10_i128.pow(Self::DECIMAL_PLACES));
    const DECIMAL_PLACES: u32 = 18;
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        crate::{dec_test, FixedPoint, Int},
        bnum::types::{I256, U256},
        std::fmt::Debug,
    };

    dec_test!( fixed_point
        inputs = {
            udec128 = {
                passing: [
                    (18_u32, 1_000_000_000_000_000_000_u128)
                ]
            }
            udec256 = {
                passing: [
                    (18_u32, U256::from(1_000_000_000_000_000_000_u128))
                ]
            }
            dec128 = {
                passing: [
                    (18_u32, 1_000_000_000_000_000_000_i128)
                ]
            }
            dec256 = {
                passing: [
                    (18_u32, I256::from(1_000_000_000_000_000_000_i128))
                ]
            }
        }
        method = |_0d, passing| {
            for (precision, decimal_fraction) in passing {

                fn t<U, FP: FixedPoint<U>>(_: FP, precision: u32, decimal_fraction: Int<U>)
                where Int<U>: PartialEq + Debug {
                    assert_eq!(FP::DECIMAL_FRACTION, decimal_fraction);
                    assert_eq!(FP::DECIMAL_PLACES, precision);
                }

                t(_0d, precision, Int::new(decimal_fraction));
            }
        }
    );
}