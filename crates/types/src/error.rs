use {
    crate::Denom,
    data_encoding::BASE64,
    grug_math::MathError,
    hex::FromHexError,
    std::{any::type_name, array::TryFromSliceError, convert::Infallible},
    thiserror::Error,
};

#[derive(Debug, Clone, Error)]
pub enum VerificationError {
    #[error("data is of incorrect length")]
    IncorrectLength,

    #[error("invalid recovery id; must be 0 or 1")]
    InvalidRecoveryId,

    #[error("signature is unauthentic")]
    Unauthentic,
}

impl VerificationError {
    /// Convert the error code received across WebAssembly FFI into a
    /// `VerificationError`.
    pub fn from_error_code(error_code: u32) -> Self {
        match error_code {
            1 => Self::IncorrectLength,
            2 => Self::InvalidRecoveryId,
            3 => Self::Unauthentic,
            _ => unreachable!("unknown verification error code: {error_code}, must be 1-3"),
        }
    }
}

#[derive(Debug, Clone, Error)]
pub enum StdError {
    /// This variant exists such that we can use `Coins` as the generic `C` in
    /// contructor methods `Message::{instantiate,execute}`, which has the trait
    /// bound: `StdError: From<<C as TryInto<Coins>>::Error>`.
    #[error(transparent)]
    Infallible(#[from] Infallible),

    #[error(transparent)]
    FromHex(#[from] FromHexError),

    #[error(transparent)]
    TryFromSlice(#[from] TryFromSliceError),

    // TODO: rename this. this means an error is thrown by the host over the FFI.
    // something like `StdError::Host` may be more appropriate.
    #[error("generic error: {0}")]
    Generic(String),

    #[error(transparent)]
    Math(#[from] MathError),

    #[error(transparent)]
    Verification(#[from] VerificationError),

    #[error("out of gas! limit: {limit}, used: {used}, comment: {comment}")]
    OutOfGas {
        limit: u64,
        used: u64,
        comment: &'static str,
    },

    #[error("invalid denom `{denom}`: {reason}")]
    InvalidDenom { denom: String, reason: &'static str },

    #[error("invalid coins: {reason}")]
    InvalidCoins { reason: String },

    #[error("invalid payment: expecting {expect} coins, found {actual}")]
    InvalidPayment { expect: usize, actual: usize },

    #[error("cannot find denom `{denom}` in coins")]
    DenomNotFound { denom: Denom },

    #[error("data not found! type: {ty}, storage key: {key}")]
    DataNotFound { ty: &'static str, key: String },

    #[error("duplicate data found! type: {ty}")]
    DuplicateData { ty: &'static str },

    #[error("cannot find iterator with ID {iterator_id}")]
    IteratorNotFound { iterator_id: i32 },

    #[error("expecting a non-zero value of type {ty}, got zero")]
    ZeroValue { ty: &'static str },

    #[error("failed to serialize! codec: {codec}, type: {ty}, reason: {reason}")]
    Serialize {
        codec: &'static str,
        ty: &'static str,
        reason: String,
    },

    #[error("failed to deserialize! codec: {codec}, type: {ty}, reason: {reason}")]
    Deserialize {
        codec: &'static str,
        ty: &'static str,
        reason: String,
    },
}

impl StdError {
    pub fn invalid_denom<D>(denom: D, reason: &'static str) -> Self
    where
        D: ToString,
    {
        Self::InvalidDenom {
            denom: denom.to_string(),
            reason,
        }
    }

    pub fn invalid_coins<R>(reason: R) -> Self
    where
        R: ToString,
    {
        Self::InvalidCoins {
            reason: reason.to_string(),
        }
    }

    pub fn invalid_payment(expect: usize, actual: usize) -> Self {
        Self::InvalidPayment { expect, actual }
    }

    pub fn data_not_found<T>(key: &[u8]) -> Self {
        Self::DataNotFound {
            ty: type_name::<T>(),
            key: BASE64.encode(key),
        }
    }

    pub fn duplicate_data<T>() -> Self {
        Self::DuplicateData {
            ty: type_name::<T>(),
        }
    }

    pub fn zero_value<T>() -> Self {
        Self::ZeroValue {
            ty: type_name::<T>(),
        }
    }

    pub fn serialize<T, R>(codec: &'static str, reason: R) -> Self
    where
        R: ToString,
    {
        Self::Serialize {
            codec,
            ty: type_name::<T>(),
            reason: reason.to_string(),
        }
    }

    pub fn deserialize<T, R>(codec: &'static str, reason: R) -> Self
    where
        R: ToString,
    {
        Self::Deserialize {
            codec,
            ty: type_name::<T>(),
            reason: reason.to_string(),
        }
    }

    pub fn generic_err<R>(reason: R) -> Self
    where
        R: ToString,
    {
        Self::Generic(reason.to_string())
    }
}

pub type StdResult<T> = core::result::Result<T, StdError>;
