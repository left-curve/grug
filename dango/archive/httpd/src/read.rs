//! Read-API building blocks shared by every projection's feeds: keyset
//! pagination, opaque cursors, and a parameter binder for hand-written SQL.
//!
//! A feed is newest-first and paginated on its **ordering tuple**, never
//! `OFFSET`: the cursor is that tuple, and "the next page" is
//! `WHERE (tuple) < $cursor`. The cursor is opaque to clients —
//! `hex(json(tuple))`, URL-safe with no reserved characters — so the wire form
//! never leaks the layout; the client treats it as a token and rolls the page's
//! `endCursor` back in as the next `after`. The cursor *shape* (which columns)
//! is the projection's own type, generic here over `C: Serialize`.
//!
//! [`Binder`] keeps the hand-written SQL and its bound values in lockstep, and
//! [`paginate`] turns the `limit + 1` fetched rows into a [`Page`] — the extra
//! row is how `hasNextPage` is known without a `COUNT`.

use {
    crate::error::ApiError,
    dango_archive_types::{Page, PageInfo},
    sea_orm::Value,
    serde::{Serialize, de::DeserializeOwned},
};

/// Page size when a feed is queried without an explicit `first`.
const DEFAULT_PAGE_SIZE: u64 = 50;
/// Hard ceiling on `first` — a page never returns more than this many rows. A
/// sane shared default; a projection that needs a different cap can clamp its
/// own `first` before building the page.
const MAX_PAGE_SIZE: u64 = 50;

/// The effective page size for a `first` argument, clamped to
/// `[0, MAX_PAGE_SIZE]`. `first = 0` is honored as an explicit empty page; only
/// an absent or negative `first` falls back to [`DEFAULT_PAGE_SIZE`].
#[must_use]
pub fn page_limit(first: Option<i32>) -> u64 {
    match first {
        Some(n) if n >= 0 => (n as u64).min(MAX_PAGE_SIZE),
        _ => DEFAULT_PAGE_SIZE,
    }
}

// ---- cursors ----

/// Encode a keyset as an opaque cursor token — `hex(json(cursor))`.
fn encode_cursor<C>(cursor: &C) -> Result<String, ApiError>
where
    C: Serialize,
{
    Ok(hex::encode(serde_json::to_vec(cursor)?))
}

/// Decode an opaque `after` cursor into its keyset `C`, or `None` for the first
/// page. A malformed cursor is a user error, surfaced as a 400.
pub fn decode_after<C>(after: Option<String>) -> Result<Option<C>, ApiError>
where
    C: DeserializeOwned,
{
    after
        .map(|raw| {
            let bytes = hex::decode(&raw)
                .map_err(|err| ApiError::bad_request(format!("invalid cursor: {err}")))?;
            serde_json::from_slice::<C>(&bytes)
                .map_err(|err| ApiError::bad_request(format!("invalid cursor: {err}")))
        })
        .transpose()
}

// ---- parameter binding ----

/// Accumulates the bound values of a hand-written, parameterized statement,
/// returning the matching `$n` placeholder for each value as it is bound — so
/// the SQL text and its value list can never drift out of order, and no
/// caller-supplied value is ever interpolated into the SQL raw.
#[derive(Default)]
pub struct Binder {
    values: Vec<Value>,
}

impl Binder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind one value, returning its `$n` placeholder.
    pub fn bind<V>(&mut self, value: V) -> String
    where
        V: Into<Value>,
    {
        self.values.push(value.into());
        format!("${}", self.values.len())
    }

    #[must_use]
    pub fn into_values(self) -> Vec<Value> {
        self.values
    }
}

// ---- page assembly ----

/// Turn up to `limit + 1` rows (newest-first) into a [`Page`]: the surplus row,
/// if any, sets `hasNextPage` and is dropped; the last kept row's keyset becomes
/// `endCursor`; each kept row is mapped to its output node.
pub fn paginate<M, C, N, Cur, Node>(
    mut rows: Vec<M>,
    limit: u64,
    cursor_of: Cur,
    node_of: Node,
) -> Result<Page<N>, ApiError>
where
    C: Serialize,
    Cur: Fn(&M) -> C,
    Node: Fn(M) -> Result<N, ApiError>,
{
    let has_next_page = rows.len() as u64 > limit;
    rows.truncate(limit as usize);

    let end_cursor = match rows.last() {
        Some(row) => Some(encode_cursor(&cursor_of(row))?),
        None => None,
    };

    let items = rows
        .into_iter()
        .map(node_of)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Page {
        items,
        page_info: PageInfo {
            has_next_page,
            end_cursor,
        },
    })
}
