//! Gateway is Dango's bridge aggregator contract. Dango will be equipped with
//! the following bridge protocols:
//!
//! - Hyperlane;
//! - Dango's own bitcoin bridge (name TBD).
//!
//! There are logics common to both protocols. Instead of duplicating them in
//! each protocol, they are extracted into the Gateway contract. These logics
//! include:
//!
//! - minting and burning of tokens;
//! - alloying;
//! - withdraw fee;
//! - withdraw rate limit.

pub mod bridge;
mod events;
mod msgs;
mod origin;
mod remote;

pub use {
    dango_hyperlane_types::{Addr32, mailbox::Domain},
    events::*,
    msgs::*,
    origin::*,
    remote::*,
};

use {
    dango_math::Udec128,
    dango_primitives::{Bounded, Part, ZeroInclusiveOneExclusive},
    std::sync::LazyLock,
};

pub type RateLimit = Bounded<Udec128, ZeroInclusiveOneExclusive>;

pub static NAMESPACE: LazyLock<Part> = LazyLock::new(|| Part::new_unchecked("bridge"));
