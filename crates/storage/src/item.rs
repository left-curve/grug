use {
    crate::{Borsh, Codec, Path},
    grug_types::{StdError, StdResult, Storage},
    std::marker::PhantomData,
};

pub struct Item<'a, T, C: Codec<T> = Borsh> {
    storage_key: &'a [u8],
    data: PhantomData<T>,
    codec: PhantomData<C>,
}

impl<'a, T, C> Item<'a, T, C>
where
    C: Codec<T>,
{
    pub const fn new(storage_key: &'a str) -> Self {
        Self {
            storage_key: storage_key.as_bytes(),
            data: PhantomData,
            codec: PhantomData,
        }
    }

    fn path(&self) -> Path<T, C> {
        Path::from_raw(self.storage_key)
    }

    pub fn exists(&self, storage: &dyn Storage) -> bool {
        self.path().exists(storage)
    }

    pub fn remove(&self, storage: &mut dyn Storage) {
        self.path().remove(storage)
    }
}

impl<'a, T, C> Item<'a, T, C>
where
    C: Codec<T>,
{
    pub fn save(&self, storage: &mut dyn Storage, data: &T) -> StdResult<()> {
        self.path().save(storage, data)
    }

    pub fn may_load(&self, storage: &dyn Storage) -> StdResult<Option<T>> {
        self.path().may_load(storage)
    }

    pub fn load(&self, storage: &dyn Storage) -> StdResult<T> {
        self.path().load(storage)
    }

    pub fn update<A, Err>(&self, storage: &mut dyn Storage, action: A) -> Result<Option<T>, Err>
    where
        A: FnOnce(Option<T>) -> Result<Option<T>, Err>,
        Err: From<StdError>,
    {
        self.path().update(storage, action)
    }
}
