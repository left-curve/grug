//! A small allow/deny filter, generic over the filtered item.

use {
    serde::{Deserialize, Serialize},
    std::{collections::HashSet, hash::Hash},
};

/// A set-membership filter expressed as **either** a whitelist or a blacklist.
///
/// Serialized as an externally tagged enum — `{ "whitelist": [...] }` or
/// `{ "blacklist": [...] }` — so a config can pick the polarity that reads best
/// for the case (a short allow-list, or "everything except these").
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WhiteOrBlackList<T: Eq + Hash> {
    /// Allow **only** the listed items (an empty list allows nothing).
    Whitelist(HashSet<T>),
    /// Allow **every** item except the listed ones (an empty list allows
    /// everything).
    Blacklist(HashSet<T>),
}

impl<T: Eq + Hash> WhiteOrBlackList<T> {
    /// Whether `item` passes the filter.
    #[must_use]
    pub fn allows(&self, item: &T) -> bool {
        match self {
            Self::Whitelist(set) => set.contains(item),
            Self::Blacklist(set) => !set.contains(item),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitelist_allows_only_listed_blacklist_allows_the_rest() {
        let white = WhiteOrBlackList::Whitelist(HashSet::from([1, 2]));
        assert!(white.allows(&1));
        assert!(!white.allows(&3));

        let black = WhiteOrBlackList::Blacklist(HashSet::from([1, 2]));
        assert!(!black.allows(&1));
        assert!(black.allows(&3));

        // Empty lists are the two identities: whitelist nothing, blacklist all.
        assert!(!WhiteOrBlackList::<i32>::Whitelist(HashSet::new()).allows(&1));
        assert!(WhiteOrBlackList::<i32>::Blacklist(HashSet::new()).allows(&1));
    }
}
