use {
    crate::{StdError, StdResult},
    borsh::{BorshDeserialize, BorshSerialize},
    serde::{de, Serialize},
    std::{collections::HashSet, hash::Hash, io, slice, vec},
};

/// A wrapper over a vector that guarantees that no element appears twice.
#[derive(Serialize, BorshSerialize, Debug, Clone, PartialEq, Eq)]
pub struct UniqueVec<T>(Vec<T>);

impl<T> UniqueVec<T> {
    pub fn into_inner(self) -> Vec<T> {
        self.0
    }

    pub fn iter(&self) -> slice::Iter<'_, T> {
        self.0.iter()
    }

    pub fn iter_mut(&mut self) -> slice::IterMut<'_, T> {
        self.0.iter_mut()
    }

    pub fn into_iter(self) -> vec::IntoIter<T> {
        self.0.into_iter()
    }
}

impl<T> TryFrom<Vec<T>> for UniqueVec<T>
where
    T: Eq + Hash,
{
    type Error = StdError;

    // Here we collect the elements into a set, and check whether the set has
    // the same length as the vector.
    // Different trait bounds are required using HashSet or BTreeSet.
    // HashSet has faster insertion and lookup, while BTreeSet has faster
    // comparison if `T` is a simple number type such as `u32`.
    // Overall, we choose to use a HashSet here.
    fn try_from(vector: Vec<T>) -> StdResult<Self> {
        let set = vector.iter().collect::<HashSet<_>>();

        if set.len() != vector.len() {
            return Err(StdError::duplicate_data::<T>());
        }

        Ok(Self(vector))
    }
}

impl<'de, T> de::Deserialize<'de> for UniqueVec<T>
where
    T: Eq + Hash + de::Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        <Vec<T> as de::Deserialize>::deserialize(deserializer)?
            .try_into()
            .map_err(de::Error::custom)
    }
}

impl<T> BorshDeserialize for UniqueVec<T>
where
    T: Eq + Hash + BorshDeserialize,
{
    fn deserialize_reader<R: io::Read>(reader: &mut R) -> io::Result<Self> {
        <Vec<T> as BorshDeserialize>::deserialize_reader(reader)?
            .try_into()
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
    }
}
