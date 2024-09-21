mod macros;

pub use {grug_macros::*, grug_math::*, grug_storage::*, grug_types::*};

// The FFI crate is only included when building for WebAssembly.
#[cfg(target_arch = "wasm32")]
pub use grug_ffi::*;

// The cleint and testing crates are only included when _not_ building for
// WebAseembly. They contain Wasm-incompatible feature, such as async runtime,
// threads, and RNGs.
#[cfg(not(target_arch = "wasm32"))]
pub use {grug_client::*, grug_testing::*};

// Dependencies used by the procedural macros
#[doc(hidden)]
pub mod __private {
    pub use {::borsh, ::paste, ::serde, ::serde_with};
}
