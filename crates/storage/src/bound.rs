use crate::Key;

/// Like Bound but only with the raw binary variants.
pub enum RawBound {
    Inclusive(Vec<u8>),
    Exclusive(Vec<u8>),
}

/// Describe the limit for iteration.
///
/// Typically we use an `Option<Bound<T>>` in contracts, where `T` implements
/// the `Key` trait.
///
/// Compared to `std::ops::Bound`, it removes the unbounded option (which is to
/// be represented by a `None`), and introduces the "raw" variants. We don't use
/// std `Bound` because it typically requires more verbose code in contracts.
pub enum Bound<K> {
    Inclusive(K),
    Exclusive(K),
    InclusiveRaw(Vec<u8>),
    ExclusiveRaw(Vec<u8>),
}

impl<K> Bound<K> {
    pub fn inclusive<T: Into<K>>(t: T) -> Self {
        Self::Inclusive(t.into())
    }

    pub fn exclusive<T: Into<K>>(t: T) -> Self {
        Self::Exclusive(t.into())
    }
}

impl<K> From<Bound<K>> for RawBound
where
    K: Key,
{
    fn from(bound: Bound<K>) -> Self {
        match bound {
            Bound::Inclusive(key) => RawBound::Inclusive(key.joined_key()),
            Bound::Exclusive(key) => RawBound::Exclusive(key.joined_key()),
            Bound::InclusiveRaw(bytes) => RawBound::Inclusive(bytes),
            Bound::ExclusiveRaw(bytes) => RawBound::Exclusive(bytes),
        }
    }
}
