use {
    crate::{Binary, StdError, StdResult},
    borsh::{BorshDeserialize, BorshSerialize},
    grug_math::Inner,
    serde::{Deserialize, Deserializer, Serialize, Serializer},
    std::{
        collections::{BTreeMap, BTreeSet},
        ops::Deref,
    },
};

pub type MaxLength<T, const MAX: usize> = LengthBounded<T, 0, MAX>;
pub type MinLength<T, const MIN: usize> = LengthBounded<T, MIN, { usize::MAX }>;
pub type NonEmpty<T> = MinLength<T, 1>;
pub type FixedLength<T, const LEN: usize> = LengthBounded<T, LEN, LEN>;

/// A wrapper that enforces the value to be within the specified length.
/// The value must implement the `LengthBounds` trait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LengthBounded<T, const MIN: usize, const MAX: usize>(T)
where
    T: Lengthy;

pub trait Lengthy {
    fn length(&self) -> usize;
}

impl<T, const MIN: usize, const MAX: usize> LengthBounded<T, MIN, MAX>
where
    T: Lengthy,
{
    pub fn new(value: T) -> StdResult<Self> {
        let length = value.length();

        if length < MIN {
            return Err(StdError::length_out_of_bound::<T>(length, "<", MIN));
        }

        if length > MAX {
            return Err(StdError::length_out_of_bound::<T>(length, ">", MAX));
        }

        Ok(Self(value))
    }

    pub fn new_unchecked(value: T) -> Self {
        Self(value)
    }
}

impl<T, const MIN: usize, const MAX: usize> Inner for LengthBounded<T, MIN, MAX>
where
    T: Lengthy,
{
    type U = T;

    fn inner(&self) -> &Self::U {
        &self.0
    }

    fn into_inner(self) -> Self::U {
        self.0
    }
}

impl<T, const MIN: usize, const MAX: usize> AsRef<T> for LengthBounded<T, MIN, MAX>
where
    T: Lengthy,
{
    fn as_ref(&self) -> &T {
        &self.0
    }
}

impl<T, const MIN: usize, const MAX: usize> Deref for LengthBounded<T, MIN, MAX>
where
    T: Lengthy,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T, const MIN: usize, const MAX: usize> Serialize for LengthBounded<T, MIN, MAX>
where
    T: Lengthy + Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de, T, const MIN: usize, const MAX: usize> Deserialize<'de> for LengthBounded<T, MIN, MAX>
where
    T: Lengthy + Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = T::deserialize(deserializer)?;

        LengthBounded::new(value).map_err(serde::de::Error::custom)
    }
}

impl<T, const MIN: usize, const MAX: usize> BorshSerialize for LengthBounded<T, MIN, MAX>
where
    T: Lengthy + BorshSerialize,
{
    fn serialize<W>(&self, writer: &mut W) -> std::io::Result<()>
    where
        W: std::io::Write,
    {
        self.0.serialize(writer)
    }
}

impl<T, const MIN: usize, const MAX: usize> BorshDeserialize for LengthBounded<T, MIN, MAX>
where
    T: Lengthy + BorshDeserialize,
{
    fn deserialize_reader<R>(reader: &mut R) -> std::io::Result<Self>
    where
        R: std::io::Read,
    {
        let value = BorshDeserialize::deserialize_reader(reader)?;

        Self::new(value).map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))
    }
}

// ---------------------------- LengthBounds impls -----------------------------

impl Lengthy for Binary {
    fn length(&self) -> usize {
        self.len()
    }
}

impl Lengthy for String {
    fn length(&self) -> usize {
        self.len()
    }
}

impl Lengthy for Vec<u8> {
    fn length(&self) -> usize {
        self.len()
    }
}

impl<const LEN: usize> Lengthy for [u8; LEN] {
    fn length(&self) -> usize {
        self.len()
    }
}

impl<K, V> Lengthy for BTreeMap<K, V> {
    fn length(&self) -> usize {
        self.len()
    }
}

impl<K> Lengthy for BTreeSet<K> {
    fn length(&self) -> usize {
        self.len()
    }
}

impl<T, U, const LEN: usize> From<[U; LEN]> for FixedLength<T, LEN>
where
    T: Lengthy,
    [U; LEN]: Into<T>,
{
    fn from(value: [U; LEN]) -> Self {
        FixedLength::new_unchecked(value.into())
    }
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::LengthBounded,
        crate::{btree_map, Binary, ResultExt},
        paste::paste,
    };

    macro_rules! valid {
        (
            $(
                {
                    value = $value:expr,
                    min   = $min:expr,
                    max   = $max:expr,
                    name  = $name:literal $(,)?
                }
            ),* $(,)?
        ) => {
            paste! {
                $(
                    #[test]
                    fn [<length_bounded_ok_ $name>]() {
                        LengthBounded::<_, $min, $max>::new($value).unwrap();
                    }
                )*
            }
        };
    }

    macro_rules! invalid {
        (
            $(
                {
                    value = $value:expr,
                    min   = $min:expr,
                    max   = $max:expr,
                    error = $error:expr,
                    name  = $name:literal $(,)?
                }
            ),* $(,)?
        ) => {
            paste! {
                $(
                    #[test]
                    fn [<length_bounded_err_ $name>]() {
                        LengthBounded::<_, $min, $max>::new($value).should_fail_with_error($error);
                    }
                )*
            }
        };
    }

    valid!(
        {
            value = "hello".to_string(),
            min = 5,
            max = 6,
            name = "string_min",
        },
        {
            value = "hello".to_string(),
            min = 4,
            max = 5,
            name = "string_max",
        },
        {
            value = "hello".to_string(),
            min = 5,
            max = 5,
            name = "string_exact",
        },
        {
            value = Binary::from([1,2,3]),
            min = 3,
            max = 5,
            name = "binary_min",
        },
        {
            value = Binary::from([1,2,3,4,5]),
            min = 3,
            max = 5,
            name = "binary_max",
        },
        {
            value = Binary::from([1,2,3,4]),
            min = 4,
            max = 4,
            name = "binary_exact",
        },
        {
            value = btree_map!("a" => 1, "b" => 2, "c" => 3),
            min = 3,
            max = 5,
            name = "btree_map_min",
        },
        {
            value = btree_map!("a" => 1, "b" => 2, "c" => 3),
            min = 1,
            max = 3,
            name = "btree_map_max",
        },
        {
            value = btree_map!("a" => 1, "b" => 2, "c" => 3, "d" => 4),
            min = 4,
            max = 4,
            name = "btree_map_exact",
        }
    );

    invalid!(
        {
            value = "hello".to_string(),
            min = 6,
            max = 8,
            error = "length of alloc::string::String out of bound: 5 < 6",
            name = "str_to_short",
        },
        {
            value = "hello".to_string(),
            min = 3,
            max = 4,
            error = "length of alloc::string::String out of bound: 5 > 4",
            name = "str_to_long",
        },
        {
            value = Binary::from([1,2,3]),
            min = 4,
            max = 5,
            error = "length of grug_types::bytes::EncodedBytes<alloc::vec::Vec<u8>, grug_types::bytes::Base64Encoder> out of bound: 3 < 4",
            name = "binary_to_short",
        },
        {
            value = Binary::from([1,2,3,4,5,6]),
            min = 4,
            max = 5,
            error = "length of grug_types::bytes::EncodedBytes<alloc::vec::Vec<u8>, grug_types::bytes::Base64Encoder> out of bound: 6 > 5",
            name = "binary_to_long",
        },
        {
            value = btree_map!("a" => 1, "b" => 2, "c" => 3),
            min = 4,
            max = 5,
            error = "length of alloc::collections::btree::map::BTreeMap<&str, i32> out of bound: 3 < 4",
            name = "btree_map_to_short",
        },
        {
            value = btree_map!("a" => 1, "b" => 2, "c" => 3, "d" => 4, "e" => 5, "f" => 6),
            min = 4,
            max = 5,
            error = "length of alloc::collections::btree::map::BTreeMap<&str, i32> out of bound: 6 > 5",
            name = "btree_map_to_long",
        },
    );
}
