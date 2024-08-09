use {
    crate::{Borsh, Codec, Path},
    std::{marker::PhantomData, ops::Deref},
};

pub struct Item<'a, T, C = Borsh>
where
    C: Codec<T>,
{
    data: PhantomData<T>,
    codec: PhantomData<C>,
    path: Path<'a, T, C>,
}

impl<'a, T, C> Item<'a, T, C>
where
    C: Codec<T>,
{
    pub const fn new(storage_key: &'a str) -> Self {
        Self {
            path: Path::from_raw(storage_key.as_bytes()),
            data: PhantomData,
            codec: PhantomData,
        }
    }
}

impl<'a, T, C: Codec<T>> Deref for Item<'a, T, C> {
    type Target = Path<'a, T, C>;

    fn deref(&self) -> &Self::Target {
        &self.path
    }
}

#[cfg(test)]
mod test {
    use {
        super::Item,
        borsh::{BorshDeserialize, BorshSerialize},
        grug_types::{to_borsh_vec, MockStorage, StdError, StdResult, Storage},
    };

    #[derive(BorshDeserialize, BorshSerialize, PartialEq, Debug)]
    struct Config {
        pub owner: String,
        pub max_tokens: i32,
    }

    const CONFIG: Item<Config> = Item::new("config");

    #[test]
    fn save_and_load() {
        let mut store = MockStorage::new();

        assert!(CONFIG.load(&store).is_err());
        assert_eq!(CONFIG.may_load(&store).unwrap(), None);

        let cfg = Config {
            owner: "admin".to_string(),
            max_tokens: 1234,
        };
        CONFIG.save(&mut store, &cfg).unwrap();

        assert_eq!(cfg, CONFIG.load(&store).unwrap());
    }

    #[test]
    fn owned_key_works() {
        let mut store = MockStorage::new();

        for (ns, v) in [("key0", 0_u32), ("key1", 1), ("key2", 2)] {
            let item: Item<u32> = Item::new(ns);
            item.save(&mut store, &v).unwrap();
        }

        assert_eq!(store.read(b"key0").unwrap(), 0_u32.to_le_bytes());
        assert_eq!(store.read(b"key1").unwrap(), 1_u32.to_le_bytes());
        assert_eq!(store.read(b"key2").unwrap(), 2_u32.to_le_bytes());
    }

    #[test]
    fn exists_works() {
        let mut store = MockStorage::new();

        assert!(!CONFIG.exists(&store));

        let cfg = Config {
            owner: "admin".to_string(),
            max_tokens: 1234,
        };
        CONFIG.save(&mut store, &cfg).unwrap();

        assert!(CONFIG.exists(&store));

        const OPTIONAL: Item<Option<u32>> = Item::new("optional");

        assert!(!OPTIONAL.exists(&store));

        OPTIONAL.save(&mut store, &None).unwrap();

        assert!(OPTIONAL.exists(&store));
    }

    #[test]
    fn remove_works() {
        let mut store = MockStorage::new();

        // store data
        let cfg = Config {
            owner: "admin".to_string(),
            max_tokens: 1234,
        };
        CONFIG.save(&mut store, &cfg).unwrap();
        assert_eq!(cfg, CONFIG.load(&store).unwrap());

        // remove it and loads None
        CONFIG.remove(&mut store);
        assert!(!CONFIG.exists(&store));

        // safe to remove 2 times
        CONFIG.remove(&mut store);
        assert!(!CONFIG.exists(&store));
    }

    #[test]
    fn isolated_reads() {
        let mut store = MockStorage::new();

        let cfg = Config {
            owner: "admin".to_string(),
            max_tokens: 1234,
        };
        CONFIG.save(&mut store, &cfg).unwrap();

        let reader = Item::<Config>::new("config");
        assert_eq!(cfg, reader.load(&store).unwrap());

        let other_reader = Item::<Config>::new("config2");
        assert_eq!(other_reader.may_load(&store).unwrap(), None);
    }

    #[test]
    fn update_success() {
        let mut store = MockStorage::new();

        CONFIG
            .update(&mut store, |c| -> StdResult<_> {
                assert!(c.is_none());

                Ok(Some(Config {
                    owner: "admin".to_string(),
                    max_tokens: 1234,
                }))
            })
            .unwrap();

        let output = CONFIG
            .update(&mut store, |mut c| -> StdResult<_> {
                c.as_mut().unwrap().max_tokens *= 2;
                Ok(c)
            })
            .unwrap();

        let expected = Config {
            owner: "admin".to_string(),
            max_tokens: 2468,
        };
        assert_eq!(output.unwrap(), expected);
        assert_eq!(CONFIG.load(&store).unwrap(), expected);
    }

    #[test]
    fn update_can_change_variable_from_outer_scope() {
        let mut store = MockStorage::new();
        let cfg = Config {
            owner: "admin".to_string(),
            max_tokens: 1234,
        };
        CONFIG.save(&mut store, &cfg).unwrap();

        let mut old_max_tokens = 0i32;
        CONFIG
            .update(&mut store, |mut c| -> StdResult<_> {
                old_max_tokens = c.as_ref().unwrap().max_tokens;
                c.as_mut().unwrap().max_tokens *= 2;
                Ok(c)
            })
            .unwrap();
        assert_eq!(old_max_tokens, 1234);
    }

    #[test]
    fn update_does_not_change_data_on_error() {
        let mut store = MockStorage::new();

        let cfg = Config {
            owner: "admin".to_string(),
            max_tokens: 1234,
        };
        CONFIG.save(&mut store, &cfg).unwrap();

        let output = CONFIG.update(&mut store, |_c| Err(StdError::zero_log()));
        match output.unwrap_err() {
            StdError::ZeroLog { .. } => {},
            err => panic!("Unexpected error: {:?}", err),
        }
        assert_eq!(CONFIG.load(&store).unwrap(), cfg);
    }

    #[test]
    fn update_supports_custom_errors() {
        #[derive(Debug)]
        enum MyError {
            Std(StdError),
            Foo,
        }

        impl From<StdError> for MyError {
            fn from(original: StdError) -> MyError {
                MyError::Std(original)
            }
        }

        let mut store = MockStorage::new();

        let cfg = Config {
            owner: "admin".to_string(),
            max_tokens: 1234,
        };
        CONFIG.save(&mut store, &cfg).unwrap();

        let res = CONFIG.update(&mut store, |mut c| {
            let c_as_ref = c.as_ref().unwrap();

            if c_as_ref.max_tokens > 5000 {
                return Err(MyError::Foo);
            }
            if c_as_ref.max_tokens > 20 {
                return Err(StdError::generic_err("broken stuff").into()); // Uses Into to convert StdError to MyError
            }
            if c_as_ref.max_tokens > 10 {
                // Uses From to convert StdError to MyError
                to_borsh_vec(c_as_ref)?;
            }
            c.as_mut().unwrap().max_tokens += 20;
            Ok(Some(c.unwrap()))
        });
        match res.unwrap_err() {
            MyError::Std(StdError::Generic { .. }) => {},
            err => panic!("Unexpected error: {:?}", err),
        }
        assert_eq!(CONFIG.load(&store).unwrap(), cfg);
    }

    #[test]
    fn readme_works() -> StdResult<()> {
        let mut store = MockStorage::new();

        // may_load returns Option<T>, so None if data is missing
        // load returns T and Err(StdError::NotFound{}) if data is missing
        let empty = CONFIG.may_load(&store)?;
        assert_eq!(None, empty);
        let cfg = Config {
            owner: "admin".to_string(),
            max_tokens: 1234,
        };
        CONFIG.save(&mut store, &cfg)?;
        let loaded = CONFIG.load(&store)?;
        assert_eq!(cfg, loaded);

        // update an item with a closure (includes read and write)
        // returns the newly saved value
        let output = CONFIG
            .update(&mut store, |mut c| -> StdResult<_> {
                c.as_mut().unwrap().max_tokens *= 2;
                Ok(c)
            })?
            .unwrap();
        assert_eq!(2468, output.max_tokens);

        // you can error in an update and nothing is saved
        let failed = CONFIG.update(&mut store, |_| -> StdResult<_> {
            Err(StdError::generic_err("failure mode"))
        });
        assert!(failed.is_err());

        // loading data will show the first update was saved
        let loaded = CONFIG.load(&store)?;
        let expected = Config {
            owner: "admin".to_string(),
            max_tokens: 2468,
        };
        assert_eq!(expected, loaded);

        // we can remove data as well
        CONFIG.remove(&mut store);
        let empty = CONFIG.may_load(&store)?;
        assert_eq!(None, empty);

        Ok(())
    }
}
