use crate::StdResult;

/// Describes a type that can be converted from or referenced as bytes.
pub trait Bytes: Sized {
    fn as_bytes(&self) -> &[u8];

    fn as_bytes_mut(&mut self) -> &mut [u8];

    fn try_from_vec(vec: Vec<u8>) -> StdResult<Self>;
}

impl Bytes for Vec<u8> {
    #[inline]
    fn as_bytes(&self) -> &[u8] {
        self.as_slice()
    }

    #[inline]
    fn as_bytes_mut(&mut self) -> &mut [u8] {
        self.as_mut_slice()
    }

    #[inline]
    fn try_from_vec(vec: Vec<u8>) -> StdResult<Self> {
        Ok(vec)
    }
}

impl<const N: usize> Bytes for [u8; N] {
    #[inline]
    fn as_bytes(&self) -> &[u8] {
        self
    }

    #[inline]
    fn as_bytes_mut(&mut self) -> &mut [u8] {
        self
    }

    #[inline]
    fn try_from_vec(vec: Vec<u8>) -> StdResult<Self> {
        vec.as_slice().try_into().map_err(Into::into)
    }
}
