use serde::{Deserialize, Serialize};

/// A page of a feed's results: the items plus the cursor of the last one.
///
/// This is the envelope every keyset-paginated read-API feed answers with —
/// serialized by the httpd's handlers, deserialized by clients (e.g. the SDK's
/// `ArchiveClient`), so the wire shape lives here, shared by both sides.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct Page<N> {
    pub items: Vec<N>,
    pub page_info: PageInfo,
}

/// Forward-pagination metadata. `hasNextPage` comes from the surplus
/// `limit + 1`-th row; `endCursor` is the cursor of the last returned item (the
/// `after` for the next page), `null` when the page is empty.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}
