use {
    dango_primitives::{Addr, Hash256},
    dango_storage::{Counter, IndexedMap, Item, MultiIndex, UniqueIndex},
    dango_types::account_factory::{AccountIndex, User, UserIndex, Username},
};

pub const CODE_HASH: Item<Hash256> = Item::new("hash");

pub const NEXT_USER_INDEX: Counter<UserIndex> = Counter::new("user_index", 0, 1);

pub const NEXT_ACCOUNT_INDEX: Counter<AccountIndex> = Counter::new("account_index", 0, 1);

pub const USERS: IndexedMap<UserIndex, User, UserIndexes> = IndexedMap::new(
    "user",
    UserIndexes {
        by_key: MultiIndex::new2(
            |_, user| user.keys.keys().copied().collect(),
            "user",
            "user__key",
        ),
        by_account: UniqueIndex::new2(
            |_, user| user.accounts.values().copied().collect(),
            "user",
            "user__account",
        ),
        by_name: UniqueIndex::new2(|_, user| vec![user.name.clone()], "user", "user__name"),
    },
);

#[dango_storage::index_list(UserIndex, User)]
pub struct UserIndexes<'a> {
    pub by_key: MultiIndex<'a, UserIndex, Hash256, User>,
    pub by_account: UniqueIndex<'a, UserIndex, Addr, User>,
    pub by_name: UniqueIndex<'a, UserIndex, Username, User>,
}
