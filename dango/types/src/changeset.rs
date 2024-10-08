use {
    serde::{
        de::{self, Error},
        Deserialize, Serialize,
    },
    std::collections::{BTreeMap, BTreeSet},
    thiserror::Error,
};

/// An error indicating there's an attempt to create an invalid [`ChangeSet`](crate::ChangeSet),
/// because the provided `add` and `remove` have a non-empty intersection.
#[derive(Debug, Error)]
#[error("invalid change set: the add and remove sets must be disjoint")]
pub struct InvalidChangeSetError;

/// A set of changes applicable to a map-like data structure.
///
/// This struct implements a custom deserialization method that ensures there's
/// no intersection between the keys to be added and those to be removed.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ChangeSet<K, V> {
    /// For adding new key-value pairs, or updating the values associated with
    /// existing keys.
    add: BTreeMap<K, V>,
    /// For removing existing keys.
    remove: BTreeSet<K>,
    // The `add` and `remove` fields are private, such that a `ChangeSet` can
    // only be created with the `new` method or via deserialization, which
    // ensures any `ChangeSet` that exists must be valid.
}

impl<K, V> ChangeSet<K, V>
where
    K: Ord,
{
    /// Create a new `ChangeSet`.
    /// Error if `add` and `remove` have an intersection.
    pub fn new(add: BTreeMap<K, V>, remove: BTreeSet<K>) -> Result<Self, InvalidChangeSetError> {
        if add.keys().any(|k| remove.contains(k)) {
            return Err(InvalidChangeSetError);
        }

        Ok(Self { add, remove })
    }

    /// Return the `add` map as a reference.
    pub fn add(&self) -> &BTreeMap<K, V> {
        &self.add
    }

    /// Consume self, return the `add` map by value.
    pub fn into_add(self) -> BTreeMap<K, V> {
        self.add
    }

    /// Return the `remove` set as a reference.
    pub fn remove(&self) -> &BTreeSet<K> {
        &self.remove
    }

    /// Consume self, return the `remove` set by value.
    pub fn into_remove(self) -> BTreeSet<K> {
        self.remove
    }
}

impl<'de, K, V> de::Deserialize<'de> for ChangeSet<K, V>
where
    K: Ord + Deserialize<'de>,
    V: Ord + Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct UncheckedChangeSet<T, U>
        where
            T: Ord,
            U: Ord,
        {
            add: BTreeMap<T, U>,
            remove: BTreeSet<T>,
        }

        let unchecked = UncheckedChangeSet::deserialize(deserializer)?;

        ChangeSet::new(unchecked.add, unchecked.remove).map_err(D::Error::custom)
    }
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        crate::ChangeSet,
        grug::{json, JsonDeExt},
    };

    #[test]
    fn deserializing_changeset() {
        // No intersection
        assert!(json!({
            "add": {
                "a": 1,
                "b": 2,
                "c": 3,
            },
            "remove": ["d", "e", "f"],
        })
        .deserialize_json::<ChangeSet<String, usize>>()
        .is_ok());

        // Has non-empty intersection
        assert!(json!({
            "add": {
                "a": 1,
                "b": 2,
                "c": 3,
            },
            "remove": ["c", "d", "e"],
        })
        .deserialize_json::<ChangeSet<String, usize>>()
        .is_err());
    }
}
