// A named module, not flattened into the root: its wire types re-exported from
// `dango_archive_types` (e.g. `PageInfo`) would collide with the GraphQL types
// glob below.
pub mod archive;
mod client;
mod keystore;
mod secret;
mod signer;
mod subscription;
mod ws;

pub use {
    archive::ArchiveClient, client::*, dango_indexer_graphql_types::*, keystore::*, secret::*,
    signer::*, subscription::*, ws::*,
};
