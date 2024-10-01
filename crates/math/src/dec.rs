use {
    crate::{
        FixedPoint, Int, Int128, Int256, IsZero, MathError, MathResult, MultiplyRatio, Number,
        NumberConst, Sign, Uint128, Uint256,
    },
    bnum::types::{I256, U256},
    borsh::{BorshDeserialize, BorshSerialize},
    serde::{de, ser},
    std::{
        cmp::Ordering,
        fmt::{self, Display, Write},
        marker::PhantomData,
        ops::{
            Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Rem, RemAssign, Sub, SubAssign,
        },
        str::FromStr,
    },
};

// ------------------------------- generic type --------------------------------

#[derive(
    BorshSerialize, BorshDeserialize, Default, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord,
)]
pub struct Dec<U>(pub(crate) Int<U>);

impl<U> Dec<U> {
    /// Create a new [`Dec`] _without_ adding decimal places.
    ///
    /// ```rust
    /// use {
    ///     grug_math::{Udec128, Uint128},
    ///     std::str::FromStr,
    /// };
    ///
    /// let uint = Uint128::new(100);
    /// let decimal = Udec128::raw(uint);
    /// assert_eq!(decimal, Udec128::from_str("0.000000000000000100").unwrap());
    /// ```
    pub const fn raw(value: Int<U>) -> Self {
        Self(value)
    }

    pub fn numerator(&self) -> &Int<U> {
        &self.0
    }
}

impl<U> Dec<U>
where
    Self: FixedPoint<U>,
    Int<U>: NumberConst + Number,
{
    pub fn checked_from_atomics<T>(atomics: T, decimal_places: u32) -> MathResult<Self>
    where
        T: Into<Int<U>>,
    {
        let atomics = atomics.into();

        let inner = match decimal_places.cmp(&Self::DECIMAL_PLACES) {
            Ordering::Less => {
                // No overflow because decimal_places < S
                let digits = Self::DECIMAL_PLACES - decimal_places;
                let factor = Int::<U>::TEN.checked_pow(digits)?;
                atomics.checked_mul(factor)?
            },
            Ordering::Equal => atomics,
            Ordering::Greater => {
                // No overflow because decimal_places > S
                let digits = decimal_places - Self::DECIMAL_PLACES;
                if let Ok(factor) = Int::<U>::TEN.checked_pow(digits) {
                    // Safe because factor cannot be zero
                    atomics.checked_div(factor).unwrap()
                } else {
                    // In this case `factor` exceeds the Int<U> range.
                    // Any  Int<U> `x` divided by `factor` with `factor > Int::<U>::MAX` is 0.
                    // Try e.g. Python3: `(2**128-1) // 2**128`
                    Int::<U>::ZERO
                }
            },
        };

        Ok(Self(inner))
    }
}

impl<U> Dec<U>
where
    Self: FixedPoint<U>,
    Int<U>: MultiplyRatio,
{
    pub fn checked_from_ratio<N, D>(numerator: N, denominator: D) -> MathResult<Self>
    where
        N: Into<Int<U>>,
        D: Into<Int<U>>,
    {
        let numerator = numerator.into();
        let denominator = denominator.into();

        numerator
            .checked_multiply_ratio_floor(Self::DECIMAL_FRACTION, denominator)
            .map(Self)
    }
}

impl<U> Neg for Dec<U>
where
    U: Neg<Output = U>,
{
    type Output = Self;

    fn neg(self) -> Self::Output {
        Self(-self.0)
    }
}

impl<U> Display for Dec<U>
where
    Self: FixedPoint<U>,
    U: Number + IsZero + Display,
    Int<U>: Copy + Sign + NumberConst + PartialEq,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let decimals = Self::DECIMAL_FRACTION;
        let whole = (self.0) / decimals;
        let fractional = (self.0).checked_rem(decimals).unwrap();

        if whole == Int::<U>::MIN && whole.is_negative() {
            f.write_str(whole.to_string().as_str())?
        }

        if fractional.is_zero() {
            write!(f, "{whole}")?;
        } else {
            let fractional_string = format!(
                "{:0>padding$}",
                fractional.checked_abs().unwrap().0,
                padding = Self::DECIMAL_PLACES as usize
            );
            if whole.is_negative() || fractional.is_negative() {
                f.write_char('-')?;
            }
            f.write_str(&whole.checked_abs().unwrap().to_string())?;
            f.write_char('.')?;
            f.write_str(fractional_string.trim_end_matches('0'))?;
        }

        Ok(())
    }
}

impl<U> FromStr for Dec<U>
where
    Self: FixedPoint<U>,
    Int<U>: NumberConst + Number + Display + FromStr,
{
    type Err = MathError;

    /// Converts the decimal string to a Dec
    /// Possible inputs: "1.23", "1", "000012", "1.123000000"
    /// Disallowed: "", ".23"
    ///
    /// This never performs any kind of rounding.
    /// More than DECIMAL_PLACES fractional digits, even zeros, result in an error.
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let mut parts_iter = input.split('.');

        let mut atomics = parts_iter
            .next()
            .unwrap() // split always returns at least one element
            .parse::<Int<U>>()
            .map_err(|_| MathError::parse_number::<Self, _, _>(input, "error parsing whole"))?
            .checked_mul(Self::DECIMAL_FRACTION)
            .map_err(|_| MathError::parse_number::<Self, _, _>(input, "value too big"))?;

        if let Some(fractional_part) = parts_iter.next() {
            let fractional = fractional_part.parse::<Int<U>>().map_err(|_| {
                MathError::parse_number::<Self, _, _>(input, "error parsing fractional")
            })?;
            let exp = (Self::DECIMAL_PLACES.checked_sub(fractional_part.len() as u32)).ok_or_else(
                || {
                    MathError::parse_number::<Self, _, _>(
                        input,
                        format!(
                            "cannot parse more than {} fractional digits",
                            Self::DECIMAL_FRACTION
                        ),
                    )
                },
            )?;

            debug_assert!(exp <= Self::DECIMAL_PLACES);

            let fractional_factor = Int::TEN.checked_pow(exp).unwrap();

            // This multiplication can't overflow because
            // fractional < 10^DECIMAL_PLACES && fractional_factor <= 10^DECIMAL_PLACES
            let fractional_part = fractional.checked_mul(fractional_factor).unwrap();

            // for negative numbers, we need to subtract the fractional part
            // We can't check if atomics is negative because -0 is positive
            atomics = if input.starts_with("-") {
                atomics.checked_sub(fractional_part)
            } else {
                atomics.checked_add(fractional_part)
            }
            .map_err(|_| MathError::parse_number::<Self, _, _>(input, "Value too big"))?;
        }

        if parts_iter.next().is_some() {
            return Err(MathError::parse_number::<Self, _, _>(
                input,
                "Unexpected number of dots",
            ));
        }

        Ok(Dec(atomics))
    }
}

impl<U> ser::Serialize for Dec<U>
where
    Self: Display,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: ser::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de, U> de::Deserialize<'de> for Dec<U>
where
    Dec<U>: FromStr,
    <Dec<U> as FromStr>::Err: Display,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_str(DecVisitor::new())
    }
}

struct DecVisitor<U> {
    _marker: PhantomData<U>,
}

impl<U> DecVisitor<U> {
    pub fn new() -> Self {
        Self {
            _marker: PhantomData,
        }
    }
}

impl<'de, U> de::Visitor<'de> for DecVisitor<U>
where
    Dec<U>: FromStr,
    <Dec<U> as FromStr>::Err: Display,
{
    type Value = Dec<U>;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("string-encoded decimal")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Dec::from_str(v).map_err(E::custom)
    }
}

impl<U> Add for Dec<U>
where
    Self: Number,
{
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        self.checked_add(rhs).unwrap_or_else(|err| panic!("{err}"))
    }
}

impl<U> Sub for Dec<U>
where
    Self: Number,
{
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        self.checked_sub(rhs).unwrap_or_else(|err| panic!("{err}"))
    }
}

impl<U> Mul for Dec<U>
where
    Self: Number,
{
    type Output = Self;

    fn mul(self, rhs: Self) -> Self::Output {
        self.checked_mul(rhs).unwrap_or_else(|err| panic!("{err}"))
    }
}

impl<U> Div for Dec<U>
where
    Self: Number,
{
    type Output = Self;

    fn div(self, rhs: Self) -> Self::Output {
        self.checked_div(rhs).unwrap_or_else(|err| panic!("{err}"))
    }
}

impl<U> Rem for Dec<U>
where
    Self: Number,
{
    type Output = Self;

    fn rem(self, rhs: Self) -> Self::Output {
        self.checked_rem(rhs).unwrap_or_else(|err| panic!("{err}"))
    }
}

impl<U> AddAssign for Dec<U>
where
    Self: Number + Copy,
{
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl<U> SubAssign for Dec<U>
where
    Self: Number + Copy,
{
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}

impl<U> MulAssign for Dec<U>
where
    Self: Number + Copy,
{
    fn mul_assign(&mut self, rhs: Self) {
        *self = *self * rhs;
    }
}

impl<U> DivAssign for Dec<U>
where
    Self: Number + Copy,
{
    fn div_assign(&mut self, rhs: Self) {
        *self = *self / rhs;
    }
}

impl<U> RemAssign for Dec<U>
where
    Self: Number + Copy,
{
    fn rem_assign(&mut self, rhs: Self) {
        *self = *self % rhs;
    }
}

// ------------------------------ concrete types -------------------------------

macro_rules! generate_decimal {
    (
        name              = $name:ident,
        inner_type        = $inner:ty,
        inner_constructor = $constructor:expr,
        base_constructor  = $base_constructor:expr,
        doc               = $doc:literal,
    ) => {
        paste::paste! {
            #[doc = $doc]
            pub type $name = Dec<$inner>;

            impl $name {
                /// Create a new [`Dec`] adding decimal places.
                ///
                /// ```rust
                /// use {
                ///     grug_math::{Udec128, Uint128},
                ///     std::str::FromStr,
                /// };
                ///
                /// let decimal = Udec128::new(100);
                /// assert_eq!(decimal, Udec128::from_str("100.0").unwrap());
                /// ```
                pub const fn new(x: $base_constructor) -> Self {
                    Self($constructor(x * [<10_$base_constructor>].pow(Self::DECIMAL_PLACES)))
                }

                pub const fn new_percent(x: $base_constructor) -> Self {
                    Self($constructor(x * [<10_$base_constructor>].pow(Self::DECIMAL_PLACES - 2)))
                }

                pub const fn new_permille(x: $base_constructor) -> Self {
                    Self($constructor(x * [<10_$base_constructor>].pow(Self::DECIMAL_PLACES - 3)))
                }

                pub const fn new_bps(x: $base_constructor) -> Self {
                    Self($constructor(x * [<10_$base_constructor>].pow(Self::DECIMAL_PLACES - 4)))
                }
            }

        }
    };
    (
        type              = Signed,
        name              = $name:ident,
        inner_type        = $inner:ty,
        inner_constructor = $constructor:expr,
        doc               = $doc:literal,
    ) => {
        generate_decimal! {
            name              = $name,
            inner_type        = $inner,
            inner_constructor = $constructor,
            base_constructor  = i128,
            doc               = $doc,
        }
    };
    (
        type              = Unsigned,
        name              = $name:ident,
        inner_type        = $inner:ty,
        inner_constructor = $constructor:expr,
        doc               = $doc:literal,
    ) => {
        generate_decimal! {
            name              = $name,
            inner_type        = $inner,
            inner_constructor = $constructor,
            base_constructor  = u128,
            doc               = $doc,
        }
    };
}

generate_decimal! {
    type              = Unsigned,
    name              = Udec128,
    inner_type        = u128,
    inner_constructor = Uint128::new,
    doc               = "128-bit unsigned fixed-point number with 18 decimal places.",
}

generate_decimal! {
    type              = Unsigned,
    name              = Udec256,
    inner_type        = U256,
    inner_constructor = Uint256::new_from_u128,
    doc               = "256-bit unsigned fixed-point number with 18 decimal places.",
}

generate_decimal! {
    type              = Signed,
    name              = Dec128,
    inner_type        = i128,
    inner_constructor = Int128::new,
    doc               = "128-bit signed fixed-point number with 18 decimal places.",
}

generate_decimal! {
    type              = Signed,
    name              = Dec256,
    inner_type        = I256,
    inner_constructor = Int256::new_from_i128,
    doc               = "256-bit signed fixed-point number with 18 decimal places.",
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        crate::{Dec128, Number, NumberConst, Udec128},
        std::str::FromStr,
    };

    #[test]
    fn t1() {
        assert_eq!(Udec128::ONE + Udec128::ONE, Udec128::new(2));

        assert_eq!(
            Udec128::new(10).checked_add(Udec128::new(20)).unwrap(),
            Udec128::new(30)
        );

        assert_eq!(
            Udec128::new(3).checked_rem(Udec128::new(2)).unwrap(),
            Udec128::from_str("1").unwrap()
        );

        assert_eq!(
            Udec128::from_str("3.5")
                .unwrap()
                .checked_rem(Udec128::new(2))
                .unwrap(),
            Udec128::from_str("1.5").unwrap()
        );

        assert_eq!(
            Udec128::from_str("3.5")
                .unwrap()
                .checked_rem(Udec128::from_str("2.7").unwrap())
                .unwrap(),
            Udec128::from_str("0.8").unwrap()
        );
    }

    #[test]
    fn neg_to_string_works() {
        assert_eq!(Dec128::new(-1).to_string(), "-1");
        assert_eq!(Dec128::new_percent(-10).to_string(), "-0.1");
        assert_eq!(Dec128::new_percent(-110).to_string(), "-1.1");
        assert_eq!(Dec128::new(1).to_string(), "1");
        assert_eq!(Dec128::new_percent(10).to_string(), "0.1");
        assert_eq!(Dec128::new_percent(110).to_string(), "1.1");
    }

    #[test]
    fn new_from_str_works() {
        assert_eq!(Dec128::from_str("0.5").unwrap(), Dec128::new_percent(50));
        assert_eq!(Dec128::from_str("1").unwrap(), Dec128::new(1));
        assert_eq!(Dec128::from_str("1.05").unwrap(), Dec128::new_percent(105));
        assert_eq!(Dec128::from_str("-0.5").unwrap(), Dec128::new_percent(-50));
        assert_eq!(Dec128::from_str("-1").unwrap(), Dec128::new(-1));
        assert_eq!(
            Dec128::from_str("-1.05").unwrap(),
            Dec128::new_percent(-105)
        );
    }

    #[test]
    fn neg_works() {
        assert_eq!(-Dec128::new_percent(-105), Dec128::new_percent(105));
        assert_eq!(-Dec128::new_percent(50), Dec128::new_percent(-50));
    }
}