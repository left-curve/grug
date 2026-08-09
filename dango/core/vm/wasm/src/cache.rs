use {
    crate::VmResult,
    clru::CLruCache,
    dango_primitives::{Hash256, Shared},
    std::num::NonZeroUsize,
    wasmer::{Engine, Module},
};

/// Data to be cached in the VM cache.
///
/// Must cache the module together with the engine that was used to built it.
/// There may be runtime errors if calling a wasm function using a different
/// engine from that was used to build the module.
type Data = (Module, Engine);

/// Statistics about the usage of the cache instance.
#[derive(Default, Debug, Clone, Copy)]
pub struct Metrics {
    pub hits: usize,
    pub misses: usize,
}

impl Metrics {
    pub fn new() -> Self {
        Self { hits: 0, misses: 0 }
    }

    pub fn increment_hits(&mut self) {
        // In practice, it's unlikely a cache is hit `usize::MAX` times, but we
        // still use saturating add to avoid panicking on overflow.
        self.hits = self.hits.saturating_add(1);
    }

    pub fn increment_misses(&mut self) {
        // Same as above, use saturating add to avoid panicking on overflow.
        self.misses = self.misses.saturating_add(1);
    }
}

/// An in-memory cache for wasm modules, so that they don't need to be re-built
/// every time the same contract is called.
#[derive(Clone)]
pub struct Cache {
    inner: Shared<CacheInner>,
}

struct CacheInner {
    lru_cache: CLruCache<Hash256, Data>,
    metrics: Metrics,
}

impl Cache {
    /// Create an empty cache with the given capacity.
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            inner: Shared::new(CacheInner {
                lru_cache: CLruCache::new(capacity),
                metrics: Metrics::new(),
            }),
        }
    }

    /// Attempt to get a cached module by hash. If not found, build the module
    /// using the given method, insert the built module into the cache, and
    /// return the module.
    pub fn get_or_build_with<B>(&self, code_hash: Hash256, builder: B) -> VmResult<Data>
    where
        B: FnOnce() -> VmResult<Data>,
    {
        self.inner.write_with(|mut inner| {
            match inner.lru_cache.get(&code_hash).cloned() {
                // Cache hit - simply clone the cached data and return.
                Some(data) => {
                    inner.metrics.increment_hits();

                    Ok(data)
                },
                // Cache miss - build the module using the given builder method;
                // insert both the module and engine to the cache.
                None => {
                    let data = builder()?;

                    inner.lru_cache.put(code_hash, data.clone());
                    inner.metrics.increment_misses();

                    Ok(data)
                },
            }
        })
    }
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        crate::{Cache, VmResult},
        dango_primitives::HashExt,
        std::num::NonZeroUsize,
        wasmer::{Engine, Module, sys::Singlepass},
    };

    const CONTRACT: &[u8] = br#"(module)"#;

    fn builder() -> VmResult<(Module, Engine)> {
        let engine = Engine::from(Singlepass::new());
        let module = Module::new(&engine, CONTRACT)?;
        Ok((module, engine))
    }

    #[test]
    fn capacity_overflow() {
        let cache = Cache::new(NonZeroUsize::new(1).unwrap());

        // Build the 1st contract. Should be a cache miss, and the data is
        // inserted into the cache.
        let hash1 = CONTRACT.sha2_256();
        cache.get_or_build_with(hash1, builder).unwrap();

        // Build the 2nd contract. Should also be a cache miss, and the data is
        // inserted. Data of the previous build should have been removed,
        // because the cache only has a capacity of 1.
        let hash2 = b"jake".sha2_256();
        cache.get_or_build_with(hash2, builder).unwrap();

        // Cache should have had 2 misses, with hash2 cached but hash1 not.
        cache.inner.read_with(|inner| {
            assert!(!inner.lru_cache.contains(&hash1));
            assert!(inner.lru_cache.contains(&hash2));
            assert_eq!(inner.lru_cache.len(), 1);
            assert_eq!(inner.metrics.hits, 0);
            assert_eq!(inner.metrics.misses, 2);
        });
    }

    #[test]
    fn get_cached() {
        let cache = Cache::new(NonZeroUsize::new(2).unwrap());

        // Build the same contract twice. 1st time should be a cache miss, 2nd
        // time should be a cache hit.
        let hash = CONTRACT.sha2_256();
        cache.get_or_build_with(hash, builder).unwrap();
        cache.get_or_build_with(hash, builder).unwrap();

        cache.inner.read_with(|inner| {
            assert!(inner.lru_cache.contains(&hash));
            assert_eq!(inner.lru_cache.len(), 1);
            assert_eq!(inner.metrics.hits, 1);
            assert_eq!(inner.metrics.misses, 1);
        });
    }
}
