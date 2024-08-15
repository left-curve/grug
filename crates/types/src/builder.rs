//! Types used in creating builder types with the _type state_ pattern, which
//! allows catching some user errors at compile time.
//!
//! See [this video](https://youtu.be/pwmIQzLuYl0) for context.

use std::marker::PhantomData;

/// Represents a builder parameter that has not yet been provided.
pub struct Undefined<T>(PhantomData<T>);

impl<T> Default for Undefined<T> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

/// Represents a builder parameter that has already been provided.
pub struct Defined<T>(T);

impl<T> Defined<T> {
    pub fn new(inner: T) -> Self {
        Self(inner)
    }

    pub fn inner(&self) -> &T {
        &self.0
    }

    pub fn inner_mut(&mut self) -> &mut T {
        &mut self.0
    }

    pub fn into_inner(self) -> T {
        self.0
    }
}

pub struct Defaulted<T>(T);

impl<T> Defaulted<T> {
    pub fn new(inner: T) -> Defaulted<T> {
        Defaulted(inner)
    }

    pub fn into_inner(self) -> T {
        self.0
    }

    pub fn inner_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

/// Represents a builder parameter that may or may not have been provided.
pub trait MaybeDefined {
    type Inner;

    fn maybe_inner(self) -> Option<Self::Inner>;
}

impl<T> MaybeDefined for Defined<T> {
    type Inner = T;

    fn maybe_inner(self) -> Option<Self::Inner> {
        Some(self.into_inner())
    }
}

impl<T> MaybeDefined for Undefined<T> {
    type Inner = T;

    fn maybe_inner(self) -> Option<Self::Inner> {
        None
    }
}

impl<T> MaybeDefined for Defaulted<T> {
    type Inner = T;

    fn maybe_inner(self) -> Option<Self::Inner> {
        Some(self.into_inner())
    }
}

pub trait MaybeInitialized {
    type Inner;
    fn initialized_inner(self) -> Self::Inner;
}

impl<T> MaybeInitialized for Defined<T> {
    type Inner = T;

    fn initialized_inner(self) -> Self::Inner {
        self.into_inner()
    }
}

impl<T> MaybeInitialized for Defaulted<T> {
    type Inner = T;

    fn initialized_inner(self) -> Self::Inner {
        self.into_inner()
    }
}
