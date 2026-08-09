//! Shared types and utilities for the archive.
//!
//! This crate has no dependencies on other `dango-archive-*` crates and
//! contains items shared between the block source and the projections.

mod activity;
mod block_data;
mod page;

pub use {
    activity::{AddressRole, Event, EventType, Transaction, UnitKind},
    block_data::{BlockData, BlockDataExt},
    page::{Page, PageInfo},
};

/// Convenience alias for `anyhow::Result<T>` used across all archive
/// crates.
pub type AnyResult<T> = anyhow::Result<T>;
