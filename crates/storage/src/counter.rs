use {
    crate::{Borsh, Codec, Item},
    grug_types::{Number, NumberConst, StdResult, Storage},
};

/// An abstraction over `Item`. Stores a single number that is monotonically
/// incremented one unit at a time.
pub struct Counter<'a, T, C = Borsh>
where
    C: Codec<T>,
{
    item: Item<'a, T, C>,
}

impl<'a, T, C> Counter<'a, T, C>
where
    C: Codec<T>,
{
    pub const fn new(storage_key: &'a str) -> Self {
        Self {
            item: Item::new(storage_key),
        }
    }

    /// Load the current counter value.
    ///
    /// Error if the counter has not been initialized.
    pub fn load(&self, storage: &dyn Storage) -> StdResult<T> {
        self.item.load(storage)
    }
}

impl<'a, T, C> Counter<'a, T, C>
where
    T: Number + NumberConst,
    C: Codec<T>,
{
    /// Initialize the incrementor to the zero value.
    ///
    /// This is typically done during contract instantiation.
    pub fn initialize(&self, storage: &mut dyn Storage) -> StdResult<()> {
        self.item.save(storage, &T::ZERO)
    }

    /// Increment the value stored by one unit; return the value _after_
    /// incrementing. If no value is stored, set it to zero and return zero.
    pub fn increment(&self, storage: &mut dyn Storage) -> StdResult<T> {
        let new_value = match self.item.may_load(storage)? {
            Some(old_value) => old_value.checked_add(T::ONE)?,
            None => T::ZERO,
        };

        self.item.save(storage, &new_value)?;

        Ok(new_value)
    }
}

#[cfg(test)]
mod tests {

    use {
        super::Counter,
        borsh::{BorshDeserialize, BorshSerialize},
        grug_types::{Dec128, Int128, MockStorage, Number, NumberConst, Uint128, Uint512},
        std::{fmt::Debug, marker::PhantomData},
        test_case::test_case,
    };

    #[test_case(PhantomData::<u8>; "u8")]
    #[test_case(PhantomData::<Uint128>; "uint128")]
    #[test_case(PhantomData::<Uint512>; "uint512")]
    #[test_case(PhantomData::<Int128>; "int128")]
    #[test_case(PhantomData::<Dec128>; "dec128")]
    fn counter<T>(_p: PhantomData<T>)
    where
        T: BorshSerialize + BorshDeserialize + NumberConst + Number + PartialEq + Debug,
    {
        let mut storage = MockStorage::new();
        let counter = Counter::<T>::new("counter");

        counter.load(&storage).unwrap_err();

        let mut asserter = T::ZERO;

        counter.initialize(&mut storage).unwrap();
        assert_eq!(counter.load(&storage).unwrap(), asserter);

        asserter = asserter.checked_add(T::ONE).unwrap();
        assert_eq!(counter.increment(&mut storage).unwrap(), asserter);
        assert_eq!(counter.load(&storage).unwrap(), asserter);

        asserter = asserter.checked_add(T::ONE).unwrap();
        assert_eq!(counter.increment(&mut storage).unwrap(), asserter);
        assert_eq!(counter.load(&storage).unwrap(), asserter);
    }
}
