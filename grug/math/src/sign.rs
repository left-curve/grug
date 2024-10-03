use {
    crate::{Dec, Int, MathError, MathResult, NumberConst},
    bnum::types::{I256, I512, U256, U512},
};

/// Describes a number that can take on negative values.
/// Zero is considered non-negative, for which this should return `false`.
pub trait Sign: Sized {
    fn checked_abs(self) -> MathResult<Self>;

    fn is_negative(&self) -> bool;
}

// ------------------------------------ int ------------------------------------

impl<U> Sign for Int<U>
where
    U: Sign,
{
    fn checked_abs(self) -> MathResult<Self> {
        self.0.checked_abs().map(Self)
    }

    fn is_negative(&self) -> bool {
        self.0.is_negative()
    }
}

// ------------------------------------ dec ------------------------------------

impl<U> Sign for Dec<U>
where
    U: Sign,
{
    fn checked_abs(self) -> MathResult<Self> {
        self.0.checked_abs().map(Self)
    }

    fn is_negative(&self) -> bool {
        self.0.is_negative()
    }
}

// ----------------------------------- unsigned ------------------------------------

macro_rules! impl_sign_unsigned {
    ($t:ty) => {
        impl Sign for $t {
            fn checked_abs(self) -> MathResult<Self> {
                Ok(self)
            }

            fn is_negative(&self) -> bool {
                false
            }
        }
    };
    ($($t:ty),+ $(,)?) => {
        $(
            impl_sign_unsigned!($t);
        )+
    };
}

impl_sign_unsigned!(u8, u16, u32, u64, u128, U256, U512);

// ---------------------------------- signed -----------------------------------

macro_rules! impl_sign_signed {
    ($t:ty) => {
        impl Sign for $t {
            fn checked_abs(self) -> MathResult<Self> {
                if self == Self::MIN {
                    Err(MathError::overflow_conversion::<_, Self>(self))
                } else {
                    Ok(self.abs())
                }


            }

            fn is_negative(&self) -> bool {
                *self < Self::ZERO
            }
        }
    };
    ($($t:ty),+ $(,)?) => {
        $(
            impl_sign_signed!($t);
        )+
    };
}

impl_sign_signed!(i8, i16, i32, i64, i128, I256, I512);

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod int_tests {
    use {
        crate::{int_test, test_utils::bt, Int, MathError, NumberConst, Sign},
        bnum::types::{I256, U256},
    };

    int_test!( sign
        inputs = {
            u128 = {
                passing: [
                    (u128::ZERO, false, u128::ZERO),
                    (u128::MAX, false, u128::MAX),
                ],
                failing: []
            }
            u256 = {
                passing: [
                    (U256::ZERO, false, U256::ZERO),
                    (U256::MAX, false, U256::MAX),
                ],
                failing: []
            }
            i128 = {
                passing: [
                    (i128::ZERO, false, i128::ZERO),
                    (i128::MAX, false, i128::MAX),
                    (-i128::ONE, true, i128::ONE),
                    (-i128::MAX, true, i128::MAX),
                ],
                failing: [
                    i128::MIN,
                ]
            }
            i256 = {
                passing: [
                    (I256::ZERO, false, I256::ZERO),
                    (I256::MAX, false, I256::MAX),
                    (-I256::ONE, true, I256::ONE),
                    (-I256::MAX, true, I256::MAX),
                ],
                failing: [
                    I256::MIN,
                ]
            }
        }
        method = |_0, passing, failing| {
            for (base, sign, abs) in passing {
                let base = bt(_0, Int::new(base));
                assert_eq!(base.is_negative(), sign);
                assert_eq!(base.checked_abs().unwrap(), Int::new(abs));
            }

            for failing in failing {
                let base = bt(_0, Int::new(failing));
                assert!(matches!(base.checked_abs(), Err(MathError::OverflowConversion { .. })));
            }
        }
    );
}

#[cfg(test)]
mod dec_tests {
    use crate::{
        dec_test,
        test_utils::{dt, Leftover},
        Dec, MathError, NumberConst, Sign,
    };

    dec_test!( sign
        inputs = {
            udec128 = {
                passing: [
                    (Dec::ZERO, false, Dec::ZERO),
                    (Dec::MAX, false, Dec::MAX),
                ],
                failing: []
            }
            udec256 = {
                passing: [
                    (Dec::ZERO, false, Dec::ZERO),
                    (Dec::MAX, false, Dec::MAX),
                ],
                failing: []
            }
            dec128 = {
                passing: [
                    (Dec::ZERO, false, Dec::ZERO),
                    (Dec::MAX, false, Dec::MAX),
                    (-Dec::ONE, true, Dec::ONE),
                    (Dec::MIN + Dec::LEFTOVER, true, Dec::MAX),
                ],
                failing: [
                    Dec::MIN,
                ]
            }
            dec256 = {
                passing: [
                    (Dec::ZERO, false, Dec::ZERO),
                    (Dec::MAX, false, Dec::MAX),
                    (-Dec::ONE, true, Dec::ONE),
                    (Dec::MIN + Dec::LEFTOVER, true, Dec::MAX),
                ],
                failing: [
                    Dec::MIN,
                ]
            }
        }
        method = |_0d: Dec<_>, passing, failing| {
            for (base, sign, abs) in passing {
                dt(_0d, base);
                assert_eq!(base.is_negative(), sign);
                assert_eq!(base.checked_abs().unwrap(), abs);
            }

            for failing in failing {
                dt(_0d, failing);
                assert!(matches!(failing.checked_abs(), Err(MathError::OverflowConversion { .. })));
            }
        }
    );
}
