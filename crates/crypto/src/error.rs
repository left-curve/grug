use grug_types::CryptoError;

pub type CryptoResult<T> = core::result::Result<T, CryptoError>;

pub(crate) trait SignatureResultExt {
    type Inner;
    fn crypto_verify_failed(self) -> CryptoResult<Self::Inner>;
    fn crypto_recovery_failed(self) -> CryptoResult<Self::Inner>;
    fn crypto_invalid_pk_format(self) -> CryptoResult<Self::Inner>;
    fn crypto_invalid_sig_format(self) -> CryptoResult<Self::Inner>;
}

impl<T> SignatureResultExt for Result<T, signature::Error> {
    type Inner = T;

    fn crypto_verify_failed(self) -> CryptoResult<T> {
        self.map_err(|_| CryptoError::VerifyFailed)
    }

    fn crypto_recovery_failed(self) -> CryptoResult<Self::Inner> {
        self.map_err(|_| CryptoError::RecoveryFailed)
    }

    fn crypto_invalid_pk_format(self) -> CryptoResult<T> {
        self.map_err(|_| CryptoError::InvalidPk)
    }

    fn crypto_invalid_sig_format(self) -> CryptoResult<T> {
        self.map_err(|_| CryptoError::InvalidSig)
    }
}
