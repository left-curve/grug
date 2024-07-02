use {
    crate::VmResult,
    clru::CLruCache,
    grug_app::Shared,
    grug_types::Hash,
    std::num::NonZeroUsize,
    wasmer::{Engine, Module},
};

#[derive(Clone)]
pub struct Cache {
    inner: Shared<CLruCache<Hash, (Module, Engine)>>,
}

impl Cache {
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            inner: Shared::new(CLruCache::new(capacity)),
        }
    }

    /// Attempt to get a cached module by hash. If not found, build the module
    /// using the given method, insert the built module into the cache, and
    /// return the module.
    pub fn get_or_build_with<B>(&self, code_hash: &Hash, builder: B) -> VmResult<(Module, Engine)>
    where
        B: FnOnce() -> VmResult<(Module, Engine)>,
    {
        if let Some(module) = self.inner.write_access().get(code_hash) {
            return Ok(module.clone());
        }

        let (module, engine) = builder()?;
        self.inner
            .write_access()
            .put(code_hash.clone(), (module.clone(), engine.clone()));

        Ok((module, engine))
    }
}
