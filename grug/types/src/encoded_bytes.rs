use {
    crate::{Bytes, Encoder, StdError, StdResult},
    borsh::{BorshDeserialize, BorshSerialize},
    grug_math::{Inner, InnerMut},
    serde::{de, ser},
    std::{
        fmt::{self, Debug, Display},
        io,
        marker::PhantomData,
        ops::{Deref, DerefMut},
        str::FromStr,
    },
};

/// A wrapper over some bytes that encodes them into a string with a specific
/// encoding scheme.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EncodedBytes<B, E>
where
    B: Bytes,
    E: Encoder,
{
    bytes: B,
    encoder: PhantomData<E>,
}

impl<B, E> EncodedBytes<B, E>
where
    B: Bytes,
    E: Encoder,
{
    pub const fn from_inner(bytes: B) -> Self {
        Self {
            bytes,
            encoder: PhantomData,
        }
    }
}

impl<'a, B, E> PartialEq<EncodedBytes<B, E>> for &'a EncodedBytes<B, E>
where
    B: Bytes,
    E: Encoder,
{
    fn eq(&self, other: &EncodedBytes<B, E>) -> bool {
        self.bytes.as_bytes() == other.bytes.as_bytes()
    }
}

impl<'a, B, E> PartialEq<&'a EncodedBytes<B, E>> for EncodedBytes<B, E>
where
    B: Bytes,
    E: Encoder,
{
    fn eq(&self, other: &&'a EncodedBytes<B, E>) -> bool {
        self.bytes.as_bytes() == other.bytes.as_bytes()
    }
}

impl<B, E> From<B> for EncodedBytes<B, E>
where
    B: Bytes,
    E: Encoder,
{
    fn from(inner: B) -> Self {
        EncodedBytes::from_inner(inner)
    }
}

impl<E, const N: usize> TryFrom<&[u8]> for EncodedBytes<[u8; N], E>
where
    E: Encoder,
{
    type Error = StdError;

    fn try_from(slice: &[u8]) -> StdResult<Self> {
        slice.try_into().map(Self::from_inner).map_err(Into::into)
    }
}

impl<E, const N: usize> TryFrom<Vec<u8>> for EncodedBytes<[u8; N], E>
where
    E: Encoder,
{
    type Error = StdError;

    fn try_from(vec: Vec<u8>) -> StdResult<Self> {
        vec.as_slice().try_into()
    }
}

impl<E, const N: usize> From<[u8; N]> for EncodedBytes<Vec<u8>, E>
where
    E: Encoder,
{
    fn from(array: [u8; N]) -> Self {
        EncodedBytes::from_inner(array.to_vec())
    }
}

impl<E> From<&[u8]> for EncodedBytes<Vec<u8>, E>
where
    E: Encoder,
{
    fn from(slice: &[u8]) -> Self {
        EncodedBytes::from_inner(slice.to_vec())
    }
}

impl<E> From<&str> for EncodedBytes<Vec<u8>, E>
where
    E: Encoder,
{
    fn from(s: &str) -> Self {
        EncodedBytes::from_inner(s.as_bytes().to_vec())
    }
}

impl<E> From<String> for EncodedBytes<Vec<u8>, E>
where
    E: Encoder,
{
    fn from(s: String) -> Self {
        EncodedBytes::from_inner(s.into_bytes())
    }
}

impl<B, E> Inner for EncodedBytes<B, E>
where
    B: Bytes,
    E: Encoder,
{
    type U = B;

    fn inner(&self) -> &Self::U {
        &self.bytes
    }

    fn into_inner(self) -> Self::U {
        self.bytes
    }
}

impl<B, E> InnerMut for EncodedBytes<B, E>
where
    B: Bytes,
    E: Encoder,
{
    fn inner_mut(&mut self) -> &mut Self::U {
        &mut self.bytes
    }
}

impl<B, E> AsRef<[u8]> for EncodedBytes<B, E>
where
    B: Bytes,
    E: Encoder,
{
    fn as_ref(&self) -> &[u8] {
        self.bytes.as_bytes()
    }
}

impl<B, E> AsMut<[u8]> for EncodedBytes<B, E>
where
    B: Bytes,
    E: Encoder,
{
    fn as_mut(&mut self) -> &mut [u8] {
        self.bytes.as_bytes_mut()
    }
}

impl<B, E> Deref for EncodedBytes<B, E>
where
    B: Bytes,
    E: Encoder,
{
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.bytes.as_bytes()
    }
}

impl<B, E> DerefMut for EncodedBytes<B, E>
where
    B: Bytes,
    E: Encoder,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.bytes.as_bytes_mut()
    }
}

impl<B, E> Display for EncodedBytes<B, E>
where
    B: Bytes,
    E: Encoder,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{}{}",
            E::PREFIX,
            E::ENCODING.encode(self.bytes.as_bytes())
        )
    }
}

impl<B, E> Debug for EncodedBytes<B, E>
where
    B: Bytes,
    E: Encoder,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{}({}{})",
            E::NAME,
            E::PREFIX,
            E::ENCODING.encode(self.bytes.as_bytes())
        )
    }
}

impl<B, E> FromStr for EncodedBytes<B, E>
where
    B: Bytes,
    E: Encoder,
{
    type Err = StdError;

    fn from_str(s: &str) -> StdResult<Self> {
        if !s.starts_with(E::PREFIX) {
            return Err(StdError::deserialize::<Self, _>(
                E::NAME,
                format!("missing prefix: expecting `{}`", E::PREFIX),
            ));
        }

        let vec = E::ENCODING.decode(&s.as_bytes()[E::PREFIX.len()..])?;
        let bytes = B::try_from_vec(vec)?;

        Ok(EncodedBytes {
            bytes,
            encoder: PhantomData,
        })
    }
}

impl<B, E> ser::Serialize for EncodedBytes<B, E>
where
    B: Bytes,
    E: Encoder,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: ser::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de, B, E> de::Deserialize<'de> for EncodedBytes<B, E>
where
    B: Bytes,
    E: Encoder,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_str(Visitor(PhantomData, PhantomData))
    }
}

struct Visitor<B, E>(PhantomData<B>, PhantomData<E>);

impl<'de, B, E> de::Visitor<'de> for Visitor<B, E>
where
    B: Bytes,
    E: Encoder,
{
    type Value = EncodedBytes<B, E>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(formatter, "a byte slice in {} encoding", E::NAME)
    }

    fn visit_str<Err>(self, value: &str) -> Result<Self::Value, Err>
    where
        Err: de::Error,
    {
        Self::Value::from_str(value).map_err(Err::custom)
    }
}

impl<B, E> BorshSerialize for EncodedBytes<B, E>
where
    B: Bytes,
    E: Encoder,
{
    fn serialize<W>(&self, writer: &mut W) -> io::Result<()>
    where
        W: io::Write,
    {
        BorshSerialize::serialize(self.bytes.as_bytes(), writer)
    }
}

impl<B, E> BorshDeserialize for EncodedBytes<B, E>
where
    B: Bytes,
    E: Encoder,
{
    fn deserialize_reader<R>(reader: &mut R) -> io::Result<Self>
    where
        R: io::Read,
    {
        let vec = <Vec<u8> as BorshDeserialize>::deserialize_reader(reader)?;
        let bytes =
            B::try_from_vec(vec).map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;

        Ok(EncodedBytes {
            bytes,
            encoder: PhantomData,
        })
    }
}
