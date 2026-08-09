use dango_primitives::FullBlock;

/// The block payload that flows through the [`BlockSource`] and is consumed by
/// projections: the full `Block` together with its `BlockOutcome`.
///
/// This is the node's own [`dango_primitives::FullBlock`], reused directly
/// rather than duplicated. Two properties fall out of that for free:
///
/// - **Wire compatibility by construction.** The `full_block` subscription and
///   the `/block/full/*` REST routes serialize `FullBlock` (`{ block, outcome }`),
///   so what we deserialize is *exactly* what the node serializes — no chance of
///   a field-name drift between a private wire struct and the node's type.
/// - **On-disk format.** `FullBlock` derives borsh, the same format the dango
///   node uses for its block cache files, so the detached `RemoteBlockSource` can
///   persist it to its raw store unchanged.
///
/// `BlockData` is kept as the indexer's domain name for the payload (the alias
/// the `BlockSource` trait and the projections speak in).
///
/// [`BlockSource`]: dango_archive_block_source::BlockSource
pub type BlockData = FullBlock;

/// `height()` ergonomics for [`BlockData`].
///
/// `FullBlock` carries no inherent methods, so the height accessor the source
/// and projections lean on lives here as an extension trait — in scope wherever
/// it is called.
pub trait BlockDataExt {
    /// The block height, taken from `block.info`.
    fn height(&self) -> u64;
}

impl BlockDataExt for BlockData {
    fn height(&self) -> u64 {
        self.block.info.height
    }
}
