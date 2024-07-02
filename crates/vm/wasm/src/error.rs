use {
    grug_app::AppError,
    grug_crypto::CryptoError,
    grug_types::StdError,
    std::string::FromUtf8Error,
    thiserror::Error,
    wasmer::{
        CompileError, DeserializeError, ExportError, InstantiationError, MemoryAccessError,
        RuntimeError, SerializeError,
    },
};

#[derive(Debug, Error)]
pub enum VmError {
    #[error(transparent)]
    Std(#[from] StdError),

    #[error(transparent)]
    FromUtf8(#[from] FromUtf8Error),

    #[error(transparent)]
    Export(#[from] ExportError),

    #[error(transparent)]
    MemoryAccess(#[from] MemoryAccessError),

    #[error(transparent)]
    Runtime(#[from] RuntimeError),

    #[error(transparent)]
    Crypto(#[from] CryptoError),

    #[error(transparent)]
    App(#[from] AppError),

    #[error(transparent)]
    WasmerSerialize(#[from] SerializeError),

    #[error(transparent)]
    WasmerDeserialize(#[from] DeserializeError),

    // The wasmer `CompileError` and `InstantiateError` are big (56 and 128 bytes,
    // respectively). We get a clippy warning if we wrap them directly here in
    // VmError (result_large_err). To avoid this, we cast them to strings instead.
    #[error("failed to instantiate Wasm module: {0}")]
    Instantiation(String),

    #[error("failed to read lock ContextData")]
    FailedReadLock,

    #[error("failed to write lock ContextData")]
    FailedWriteLock,

    #[error("memory is not set in Environment")]
    MemoryNotSet,

    #[error("store is not set in ContextData")]
    StoreNotSet,

    #[error("wasmer instance is not set in ContextData")]
    WasmerInstanceNotSet,

    #[error("iterator with ID `{iterator_id}` not found")]
    IteratorNotFound { iterator_id: i32 },

    #[error("region is too small! offset: {offset}, capacity: {capacity}, data: {data}")]
    RegionTooSmall {
        offset: u32,
        capacity: u32,
        data: String,
    },

    #[error("unexpected return value count! name: {name}, expect: {expect}, actual: {actual}")]
    ReturnCount {
        name: String,
        expect: usize,
        actual: usize,
    },

    #[error("unexpected return type: {0}")]
    ReturnType(&'static str),
}

impl From<CompileError> for VmError {
    fn from(err: CompileError) -> Self {
        Self::Instantiation(err.to_string())
    }
}

impl From<InstantiationError> for VmError {
    fn from(err: InstantiationError) -> Self {
        Self::Instantiation(err.to_string())
    }
}

// required such that VmError can be used in import function signatures
impl From<VmError> for RuntimeError {
    fn from(err: VmError) -> Self {
        RuntimeError::new(err.to_string())
    }
}

impl From<VmError> for AppError {
    fn from(err: VmError) -> Self {
        AppError::Vm(err.to_string())
    }
}

pub type VmResult<T> = core::result::Result<T, VmError>;
