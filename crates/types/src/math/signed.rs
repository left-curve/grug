use {
    crate::{
        forward_ref_binop_typed, forward_ref_op_assign_typed, generate_signed,
        impl_all_ops_and_assign, impl_assign_number, impl_number, Fraction, Inner,
        MultiplyFraction, NonZero, Number, NumberConst, Sign, StdError, StdResult, Udec128,
        Udec256, Uint, Uint128, Uint256, Uint64,
    },
    borsh::{BorshDeserialize, BorshSerialize},
    forward_ref::{forward_ref_binop, forward_ref_op_assign},
    serde::{Deserialize, Serialize},
    std::{
        cmp::Ordering,
        fmt::Display,
        ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign},
        str::FromStr,
    },
};

// ------------------------------- generic type --------------------------------

#[derive(Serialize, Deserialize, BorshSerialize, BorshDeserialize, Default, Debug, Clone, Copy)]
pub struct Signed<T> {
    /// The number's absolute value. Should be an _unsigned_ number type, such
    /// as `Uint<T>` or `Udec<T>`.
    pub(crate) abs: T,
    /// Whether this number is negative. Irrelevant if absolute value is zero.
    ///
    /// This means zero can have two valid representations: `+0` and `-0`.
    /// We must be careful when implementing the `PartialEq` trait to account
    /// for this situation.
    pub(crate) negative: bool,
}

impl<T> Signed<T> {
    pub const fn new(abs: T, negative: bool) -> Self {
        Self { abs, negative }
    }

    pub const fn new_positive(abs: T) -> Self {
        Self {
            abs,
            negative: false,
        }
    }

    pub const fn new_negative(abs: T) -> Self {
        Self {
            abs,
            negative: true,
        }
    }

    pub fn inner(&self) -> &T {
        &self.abs
    }
}

impl<T> Inner for Signed<T> {
    type U = T;
}

impl<T> Sign for Signed<T> {
    fn abs(self) -> Self {
        Self::new_positive(self.abs)
    }

    fn is_negative(&self) -> bool {
        self.negative
    }
}

impl<T, AsT> Fraction<AsT> for Signed<T>
where
    T: Fraction<AsT>,
{
    fn numerator(&self) -> Uint<AsT> {
        self.abs.numerator()
    }

    fn denominator() -> NonZero<Uint<AsT>> {
        T::denominator()
    }
}

impl<T> NumberConst for Signed<T>
where
    T: NumberConst,
{
    const MAX: Self = Self::new_positive(T::MAX);
    const MIN: Self = Self::new_negative(T::MAX);
    const ONE: Self = Self::new_positive(T::ONE);
    const TEN: Self = Self::new_positive(T::TEN);
    const ZERO: Self = Self::new_positive(T::ZERO);
}

impl<T> Number for Signed<T>
where
    T: NumberConst + Number + Copy + PartialOrd + Sub<Output = T>,
    Self: Display,
{
    fn is_zero(&self) -> bool {
        self.abs.is_zero()
    }

    fn abs(self) -> Self {
        Self::new_positive(self.abs)
    }

    fn checked_add(self, other: Self) -> StdResult<Self> {
        match (self.is_negative(), other.is_negative()) {
            (false, false) => self.abs.checked_add(other.abs).map(Self::new_positive),
            (false, true) => {
                if self.abs > other.abs {
                    self.abs.checked_sub(other.abs).map(Self::new_positive)
                } else {
                    other.abs.checked_sub(self.abs).map(Self::new_negative)
                }
            },
            (true, false) => {
                if self.abs > other.abs {
                    self.abs.checked_sub(other.abs).map(Self::new_negative)
                } else {
                    other.abs.checked_sub(self.abs).map(Self::new_positive)
                }
            },
            (true, true) => self.abs.checked_add(other.abs).map(Self::new_negative),
        }
    }

    fn checked_sub(self, other: Self) -> StdResult<Self> {
        match (self.is_negative(), other.is_negative()) {
            (false, false) => {
                if self.abs > other.abs {
                    self.abs.checked_sub(other.abs).map(Self::new_positive)
                } else {
                    other.abs.checked_sub(self.abs).map(Self::new_negative)
                }
            },
            (false, true) => self.abs.checked_add(other.abs).map(Self::new_positive),
            (true, false) => self.abs.checked_add(other.abs).map(Self::new_negative),
            (true, true) => {
                if self.abs > other.abs {
                    self.abs.checked_sub(other.abs).map(Self::new_negative)
                } else {
                    other.abs.checked_sub(self.abs).map(Self::new_positive)
                }
            },
        }
    }

    fn checked_mul(self, other: Self) -> StdResult<Self> {
        self.abs
            .checked_mul(other.abs)
            .map(|inner| Self::new(inner, self.is_negative() != other.is_negative()))
    }

    fn checked_div(self, other: Self) -> StdResult<Self> {
        self.abs
            .checked_div(other.abs)
            .map(|inner| Self::new(inner, self.is_negative() != other.is_negative()))
    }

    /// On signed number, the remainder sign is the same as the dividend.
    fn checked_rem(self, other: Self) -> StdResult<Self> {
        self.abs
            .checked_rem(other.abs)
            .map(|val| Self::new(val, self.is_negative()))
    }

    fn checked_pow(self, other: u32) -> StdResult<Self> {
        // If the exponent is even, the result must be non-negative; otherwise,
        // the result has the same sign as the base.
        let negative = if other % 2 == 0 {
            false
        } else {
            self.is_negative()
        };
        self.abs
            .checked_pow(other)
            .map(|inner| Self::new(inner, negative))
    }

    fn wrapping_add(self, other: Self) -> Self {
        match (self.is_negative(), other.is_negative()) {
            // + + + => + + | Wrapping is possible
            (false, false) => {
                let result = self.abs.wrapping_add(other.abs);
                // Wrapped occurred
                if result < self.abs {
                    Self::new_negative(T::MAX - result)
                } else {
                    Self::new_positive(result)
                }
            },
            // + + - => + - | Wrapping is not possible
            (false, true) => {
                if self.abs > other.abs {
                    Self::new_positive(self.abs - other.abs)
                } else {
                    Self::new_negative(other.abs - self.abs)
                }
            },
            // - + + => - + | Wrapping is not possible
            (true, false) => {
                if self.abs > other.abs {
                    Self::new_negative(self.abs - other.abs)
                } else {
                    Self::new_positive(other.abs - self.abs)
                }
            },
            // - + - => - - | Wrapping is possible
            (true, true) => {
                let result = self.abs.wrapping_add(other.abs);
                // Wrapped occurred
                if result < self.abs {
                    Self::new_positive(T::MAX - result)
                } else {
                    Self::new_negative(result)
                }
            },
        }
    }

    fn wrapping_sub(self, other: Self) -> Self {
        match (self.is_negative(), other.is_negative()) {
            // + - + => + - | Wrapping is not possible
            (false, false) => {
                if self.abs > other.abs {
                    Self::new_positive(self.abs - other.abs)
                } else {
                    Self::new_negative(other.abs - self.abs)
                }
            },
            // + - - => + + | Wrapping is possible
            (false, true) => {
                let result = self.abs.wrapping_add(other.abs);
                // Wrapped occurred
                if result < self.abs {
                    Self::new_negative(T::MAX - result)
                } else {
                    Self::new_positive(result)
                }
            },
            // - - + => - - | Wrapping is possible
            (true, false) => {
                let result = self.abs.wrapping_add(other.abs);
                // Wrapped occurred
                if result < self.abs {
                    Self::new_positive(T::MAX - result)
                } else {
                    Self::new_negative(result)
                }
            },
            // - - - => - + | Wrapping is not possible
            (true, true) => {
                if self.abs > other.abs {
                    Self::new_negative(self.abs - other.abs)
                } else {
                    Self::new_positive(other.abs - self.abs)
                }
            },
        }
    }

    fn wrapping_mul(self, other: Self) -> Self {
        let result = self.abs.wrapping_mul(other.abs);
        Self::new(result, self.is_negative() != other.is_negative())
    }

    fn wrapping_pow(self, other: u32) -> Self {
        // If the exponent is even, the result must be non-negative; otherwise,
        // the result has the same sign as the base.
        let negative = if other % 2 == 0 {
            true
        } else {
            self.is_negative()
        };
        let result = self.abs.wrapping_pow(other);
        Self::new(result, negative)
    }

    fn saturating_add(self, other: Self) -> Self {
        match (self.is_negative(), other.is_negative()) {
            // + + + => + + | Saturing is possible
            (false, false) => {
                let result = self.abs.saturating_add(other.abs);
                Self::new_positive(result)
            },
            // + + - => + - | Saturing is not possible
            (false, true) => {
                if self.abs > other.abs {
                    Self::new_positive(self.abs - other.abs)
                } else {
                    Self::new_negative(other.abs - self.abs)
                }
            },
            // - + + => - + | Saturing is not possible
            (true, false) => {
                if self.abs > other.abs {
                    Self::new_negative(self.abs - other.abs)
                } else {
                    Self::new_positive(other.abs - self.abs)
                }
            },
            // - + - => - - | Saturing is possible
            (true, true) => {
                let result = self.abs.saturating_add(other.abs);
                Self::new_negative(result)
            },
        }
    }

    fn saturating_sub(self, other: Self) -> Self {
        match (self.is_negative(), other.is_negative()) {
            // + - + => + - | Saturing is not possible
            (false, false) => {
                if self.abs > other.abs {
                    Self::new_positive(self.abs - other.abs)
                } else {
                    Self::new_negative(other.abs - self.abs)
                }
            },
            // + - - => + + | Saturing is possible
            (false, true) => {
                let result = self.abs.saturating_mul(other.abs);
                Self::new_positive(result)
            },
            // - - + => - - | Saturing is possible
            (true, false) => {
                let result = self.abs.saturating_add(other.abs);
                Self::new_negative(result)
            },
            // - - - => - + | Saturing is not possible
            (true, true) => {
                if self.abs > other.abs {
                    Self::new_negative(self.abs - other.abs)
                } else {
                    Self::new_positive(other.abs - self.abs)
                }
            },
        }
    }

    fn saturating_mul(self, other: Self) -> Self {
        let result = self.abs.saturating_mul(other.abs);
        Self::new(result, self.is_negative() != other.is_negative())
    }

    fn saturating_pow(self, other: u32) -> Self {
        // If the exponent is even, the result must be non-negative; otherwise,
        // the result has the same sign as the base.
        let negative = if other % 2 == 0 {
            true
        } else {
            self.is_negative()
        };
        let result = self.abs.saturating_pow(other);
        Self::new(result, negative)
    }

    fn checked_sqrt(self) -> crate::StdResult<Self> {
        // Cannot take square root of a negative number.
        // This should work for `-0` though.
        if self.is_negative() && !self.is_zero() {
            return Err(crate::StdError::negative_sqrt::<Self>(self));
        }

        self.abs.checked_sqrt().map(Self::new_positive)
    }
}

// NOTE: When a negative number is floored, it should round to the negative side,
// e.g. floor(-3.14) is -4, not -3. So we should be ceiling the absolute value
// in this case.
impl<T, AsT, F> MultiplyFraction<F, AsT> for Signed<T>
where
    T: MultiplyFraction<F, AsT>,
    F: Fraction<AsT> + Sign,
    AsT: NumberConst + Number,
{
    fn checked_mul_dec_floor(self, rhs: F) -> StdResult<Self> {
        if self.negative == rhs.is_negative() {
            self.abs
                .checked_mul_dec_floor(rhs.abs())
                .map(Self::new_positive)
        } else {
            self.abs
                .checked_mul_dec_ceil(rhs.abs())
                .map(Self::new_negative)
        }
    }

    fn checked_mul_dec_ceil(self, rhs: F) -> StdResult<Self> {
        if self.negative == rhs.is_negative() {
            self.abs
                .checked_mul_dec_ceil(rhs.abs())
                .map(Self::new_positive)
        } else {
            self.abs
                .checked_mul_dec_floor(rhs.abs())
                .map(Self::new_negative)
        }
    }

    fn checked_div_dec_floor(self, rhs: F) -> StdResult<Self> {
        if self.negative == rhs.is_negative() {
            self.abs
                .checked_div_dec_floor(rhs.abs())
                .map(Self::new_positive)
        } else {
            self.abs
                .checked_div_dec_ceil(rhs.abs())
                .map(Self::new_negative)
        }
    }

    fn checked_div_dec_ceil(self, rhs: F) -> StdResult<Self> {
        if self.negative == rhs.is_negative() {
            self.abs
                .checked_div_dec_ceil(rhs.abs())
                .map(Self::new_positive)
        } else {
            self.abs
                .checked_div_dec_floor(rhs.abs())
                .map(Self::new_negative)
        }
    }
}

impl<T> Display for Signed<T>
where
    T: Number + Display,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_negative() {
            write!(f, "-{}", self.abs)
        } else {
            write!(f, "{}", self.abs)
        }
    }
}

impl<T> FromStr for Signed<T>
where
    T: FromStr,
    StdError: From<<T as FromStr>::Err>,
{
    type Err = StdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(s) = s.strip_prefix('-') {
            T::from_str(s).map(Self::new_negative).map_err(Into::into)
        } else {
            T::from_str(s).map(Self::new_positive).map_err(Into::into)
        }
    }
}

impl<T> Neg for Signed<T> {
    type Output = Self;

    fn neg(self) -> Self::Output {
        Self::new(self.abs, !self.negative)
    }
}

impl<T> PartialEq for Signed<T>
where
    T: Number + PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        // Zeroes are always equal, regardless of the sign (+0 = -0)
        if self.abs.is_zero() && other.abs.is_zero() {
            return true;
        }
        self.abs == other.abs && self.negative == other.negative
    }
}

impl<T> Eq for Signed<T> where T: Number + PartialEq {}

impl<T> PartialOrd for Signed<T>
where
    T: Number + Ord,
{
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Signed<T>
where
    T: Number + Ord,
{
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.is_negative(), other.is_negative()) {
            (false, false) => self.abs.cmp(&other.abs),
            (false, true) => {
                if self.abs.is_zero() && other.abs.is_zero() {
                    Ordering::Equal
                } else {
                    Ordering::Greater
                }
            },
            (true, false) => {
                if self.abs.is_zero() && other.abs.is_zero() {
                    Ordering::Equal
                } else {
                    Ordering::Less
                }
            },
            (true, true) => other.abs.cmp(&self.abs),
        }
    }
}

impl_number!(impl Signed with Add, add for Signed<T> where sub fn checked_add);
impl_number!(impl Signed with Sub, sub for Signed<T> where sub fn checked_sub);
impl_number!(impl Signed with Mul, mul for Signed<T> where sub fn checked_mul);
impl_number!(impl Signed with Div, div for Signed<T> where sub fn checked_div);

impl_assign_number!(impl Signed with AddAssign, add_assign for Signed<T> where sub fn checked_add);
impl_assign_number!(impl Signed with SubAssign, sub_assign for Signed<T> where sub fn checked_sub);
impl_assign_number!(impl Signed with MulAssign, mul_assign for Signed<T> where sub fn checked_mul);
impl_assign_number!(impl Signed with DivAssign, div_assign for Signed<T> where sub fn checked_div);

forward_ref_binop_typed!(impl<T> Add, add for Signed<T>, Signed<T>);
forward_ref_binop_typed!(impl<T> Sub, sub for Signed<T>, Signed<T>);
forward_ref_binop_typed!(impl<T> Mul, mul for Signed<T>, Signed<T>);
forward_ref_binop_typed!(impl<T> Div, div for Signed<T>, Signed<T>);
forward_ref_binop_typed!(impl<T> Rem, rem for Signed<T>, Signed<T>);

forward_ref_op_assign_typed!(impl<T> AddAssign, add_assign for Signed<T>, Signed<T>);
forward_ref_op_assign_typed!(impl<T> SubAssign, sub_assign for Signed<T>, Signed<T>);
forward_ref_op_assign_typed!(impl<T> MulAssign, mul_assign for Signed<T>, Signed<T>);
forward_ref_op_assign_typed!(impl<T> DivAssign, div_assign for Signed<T>, Signed<T>);

// TODO: missing implementations for Signed:
// - impl<T> Integer for Signed<T> where T: Integer
// - impl<T> Decimal for Signed<T> where T: Decimal
// - impl<T> MultiplyRatio for Signd<T> where T: MultiplyRatio
// - Signed<Uint<T>>: full_mul
// - Signed<Udec<T>>: one, decimal_fraction, new_percent/permille/bps, checked_from_atomics/ratio

// ------------------------------ concrete types -------------------------------

generate_signed!(
    name = Int64,
    inner_type = Uint64,
    from_signed = [],
    from_std = [u8, u16, u32],
    doc = "64-bit signed integer.",
);

generate_signed!(
    name = Int128,
    inner_type = Uint128,
    from_signed = [Int64],
    from_std = [u8, u16, u32],
    doc = "128-bit signed integer.",
);

generate_signed!(
    name = Int256,
    inner_type = Uint256,
    from_signed = [Int64, Int128],
    from_std = [u8, u16, u32],
    doc = "256-bit signed integer.",
);

generate_signed!(
    name = Dec128,
    inner_type = Udec128,
    from_signed = [],
    from_std = [],
    doc = "128-bit signed fixed-point number with 18 decimal places.",
);

generate_signed!(
    name = Dec256,
    inner_type = Udec256,
    from_signed = [Dec128],
    from_std = [],
    doc = "256-bit signed fixed-point number with 18 decimal places.",
);

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod test {
    use {
        super::*,
        crate::{Dec128, Dec256, Int128, Udec128},
        std::str::FromStr,
        test_case::test_case,
    };

    #[test]
    fn t1_ops() {
        let a = Int128::new_positive(10_u128.into());
        let b = Int128::new_positive(20_u128.into());

        assert_eq!(a + b, Int128::new_positive(30_u128.into()));
        assert_eq!(a - b, Int128::new_negative(10_u128.into()));
        assert_eq!(b - a, Int128::new_positive(10_u128.into()));

        assert_eq!(a * b, Int128::new_positive(200_u128.into()));
        assert_eq!(b * a, Int128::new_positive(200_u128.into()));

        let a = Int128::new_negative(10_u128.into());
        let b = Int128::new_positive(20_u128.into());

        assert_eq!(a + b, Int128::new_positive(10_u128.into()));
        assert_eq!(a - b, Int128::new_negative(30_u128.into()));
        assert_eq!(b - a, Int128::new_positive(30_u128.into()));

        assert_eq!(a * b, Int128::new_negative(200_u128.into()));
        assert_eq!(b * a, Int128::new_negative(200_u128.into()));

        let a = Int128::new_negative(10_u128.into());
        let b = Int128::new_negative(20_u128.into());

        assert_eq!(a * b, Int128::new_positive(200_u128.into()));
        assert_eq!(b * a, Int128::new_positive(200_u128.into()));
        assert_eq!(
            a.checked_pow(2).unwrap(),
            Int128::new_positive(100_u128.into())
        );
        assert_eq!(
            a.checked_pow(3).unwrap(),
            Int128::new_negative(1000_u128.into())
        );

        let a = Dec128::new_positive(Udec128::from_str("10").unwrap());
        let b = Dec128::new_negative(Udec128::from_str("10").unwrap());

        assert_eq!(a + b, Dec128::ZERO);
        assert_eq!(a + a, Dec128::from_str("20").unwrap());
        assert_eq!(b + b, Dec128::from_str("-20").unwrap());

        assert!(a > b);
        assert!(b < a);
        assert_eq!(
            Dec128::new_positive(Udec128::new(0_u128)),
            Dec128::new_negative(Udec128::new(0_u128))
        );

        let a = Int128::new_negative(10_u128.into());
        assert_eq!(a + 10_u128, Int128::new_negative(0_u128.into()));
        assert_eq!(a + 10_u64, Int128::new_negative(0_u128.into()));
    }

    #[test]
    fn t2_wrapping() {
        assert_eq!(Int128::MAX.wrapping_add(Int128::ONE), Int128::MIN);
        assert_eq!(Int128::MIN.wrapping_add(-Int128::ONE), Int128::MAX);
        assert_eq!(Int128::MIN.wrapping_sub(Int128::ONE), Int128::MAX);
        assert_eq!(Int128::MAX.wrapping_sub(-Int128::ONE), Int128::MIN);

        assert_eq!(Int128::MAX.saturating_add(Int128::ONE), Int128::MAX);
        assert_eq!(Int128::MIN.saturating_add(-Int128::ONE), Int128::MIN);
        assert_eq!(Int128::MIN.saturating_sub(Int128::ONE), Int128::MIN);
        assert_eq!(Int128::MAX.saturating_sub(-Int128::ONE), Int128::MAX);
    }

    #[test]
    fn t3_conversion() {
        let foo = Dec128::from_str("-10.5").unwrap();
        assert_eq!(
            foo,
            Dec128::new_negative(Udec128::from_str("10.5").unwrap())
        );
    }

    #[test]
    fn t4_serde() {
        let foo = Dec128::from_str("-10.5").unwrap();
        let ser = serde_json::to_string(&foo).unwrap();
        let des: Dec128 = serde_json::from_str(&ser).unwrap();
        assert_eq!(foo, des);

        let foo = Dec256::from_str("-10.5").unwrap();
        let ser = serde_json::to_string(&foo).unwrap();
        let des: Dec256 = serde_json::from_str(&ser).unwrap();
        assert_eq!(foo, des);

        let ser = borsh::to_vec(&foo).unwrap();
        let des: Dec256 = borsh::from_slice(&ser).unwrap();
        assert_eq!(foo, des);
    }

    #[test_case(
        Int128::from_str("10").unwrap(),
        Dec128::from_str("3.14").unwrap(),
        Int128::from_str("31").unwrap();
        "positive_times_positive"
    )]
    #[test_case(
        Int128::from_str("-10").unwrap(),
        Dec128::from_str("3.14").unwrap(),
        Int128::from_str("-32").unwrap();
        "negative_times_positive"
    )]
    #[test_case(
        Int128::from_str("10").unwrap(),
        Dec128::from_str("-3.14").unwrap(),
        Int128::from_str("-32").unwrap();
        "positive_times_negative"
    )]
    #[test_case(
        Int128::from_str("-10").unwrap(),
        Dec128::from_str("-3.14").unwrap(),
        Int128::from_str("31").unwrap();
        "negative_times_negative"
    )]
    fn signed_mul_dec_floor(lhs: Int128, rhs: Dec128, out: Int128) {
        assert_eq!(lhs.checked_mul_dec_floor(rhs).unwrap(), out);
    }
}
