//! Client for the **Archive API** — the archive node's REST read surface
//! (structured history: raw blocks, transaction and event feeds), the
//! counterpart of [`HttpClient`](crate::HttpClient) which talks to the Live
//! API.
//!
//! One method per route. The feed responses deserialize into the same
//! [`dango_archive_types`] structs the server serializes, so the wire format
//! cannot drift between the two sides. Every feed is newest-first and
//! keyset-paginated (`first` / `after`); the `paginate_*` conveniences walk a
//! feed to exhaustion by rolling each page's `endCursor` back in as the next
//! `after`.

pub use dango_archive_types::{
    AddressRole, BlockData, Event, EventType, Page, PageInfo, Transaction, UnitKind,
};
use {
    crate::client::error_for_status,
    anyhow::{anyhow, bail},
    async_trait::async_trait,
    dango_primitives::{Addr, Block, BlockClient, BlockOutcome, Hash256},
    reqwest::{IntoUrl, StatusCode},
    serde::{Serialize, de::DeserializeOwned},
    std::future::Future,
    url::Url,
};

#[derive(Debug, Clone)]
pub struct ArchiveClient {
    inner: reqwest::Client,
    url: Url,
}

impl ArchiveClient {
    pub fn new<U>(url: U) -> anyhow::Result<Self>
    where
        U: IntoUrl,
    {
        Ok(Self {
            inner: reqwest::Client::new(),
            url: url.into_url()?,
        })
    }

    /// GET `path` with the given query pairs, deserializing the JSON response.
    async fn get<T>(&self, path: &str, query: &[(&str, String)]) -> anyhow::Result<T>
    where
        T: DeserializeOwned,
    {
        let mut request = self.inner.get(self.url.join(path)?);
        if !query.is_empty() {
            request = request.query(query);
        }
        Ok(error_for_status(request.send().await?)
            .await?
            .json()
            .await?)
    }

    /// GET `path`, mapping a 404 to `None` — for the block reads, where an
    /// absent height is an expected state (below the backfill floor, or a cold
    /// archive), not an error.
    async fn get_opt<T>(&self, path: &str) -> anyhow::Result<Option<T>>
    where
        T: DeserializeOwned,
    {
        let response = self.inner.get(self.url.join(path)?).send().await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(error_for_status(response).await?.json().await?))
    }

    // ---- blocks ----

    /// `GET /up` — the liveness probe. `Ok` when the service answers.
    pub async fn up(&self) -> anyhow::Result<()> {
        error_for_status(self.inner.get(self.url.join("up")?).send().await?).await?;
        Ok(())
    }

    /// `GET /blocks/{height}` — the full block at `height` (`{block, outcome}`),
    /// or `None` when the archive does not hold it.
    pub async fn block(&self, height: u64) -> anyhow::Result<Option<BlockData>> {
        self.get_opt(&format!("blocks/{height}")).await
    }

    /// `GET /blocks/latest` — the block at the archive's **contiguous
    /// frontier**: the newest block servable together with all the history
    /// below it. `None` while the archive is cold (no contiguous prefix yet).
    pub async fn latest_block(&self) -> anyhow::Result<Option<BlockData>> {
        self.get_opt("blocks/latest").await
    }

    // ---- transactions ----

    /// `GET /transactions/{hash}` — every unit whose transaction bytes hash to
    /// `hash`, newest-first, un-paginated (the hash is not unique:
    /// byte-identical re-submissions can recur in later blocks).
    pub async fn transactions_by_hash(&self, hash: Hash256) -> anyhow::Result<Vec<Transaction>> {
        self.get(&format!("transactions/{hash}"), &[]).await
    }

    /// `GET /transactions/involving/{address}` — one page of the units the
    /// address **sent** or **participated in** (the union by default), narrowed
    /// with `role` / `kind`. Newest-first.
    pub async fn transactions_involving(
        &self,
        address: Addr,
        role: Option<AddressRole>,
        kind: Option<UnitKind>,
        first: Option<u32>,
        after: Option<String>,
    ) -> anyhow::Result<Page<Transaction>> {
        let mut query = Vec::new();
        if let Some(role) = role {
            query.push(("role", snake(&role)?));
        }
        if let Some(kind) = kind {
            query.push(("kind", snake(&kind)?));
        }
        push_page_args(&mut query, first, after);
        self.get(&format!("transactions/involving/{address}"), &query)
            .await
    }

    /// Walk [`transactions_involving`](Self::transactions_involving) to
    /// exhaustion, collecting all items across pages.
    pub async fn paginate_transactions_involving(
        &self,
        address: Addr,
        role: Option<AddressRole>,
        kind: Option<UnitKind>,
        page_size: Option<u32>,
    ) -> anyhow::Result<Vec<Transaction>> {
        collect_pages(|after| self.transactions_involving(address, role, kind, page_size, after))
            .await
    }

    // ---- events ----

    /// `GET /events` — one page of events filtered by `types` and/or
    /// `involved` (a participant address). The server requires **at least one**
    /// of the two (an unfiltered feed has no index anchor); an empty `types`
    /// slice omits the `type` argument. Newest-first.
    pub async fn events(
        &self,
        types: &[EventType],
        involved: Option<Addr>,
        first: Option<u32>,
        after: Option<String>,
    ) -> anyhow::Result<Page<Event>> {
        let mut query = Vec::new();
        if !types.is_empty() {
            let list = types.iter().map(snake).collect::<Result<Vec<_>, _>>()?;
            query.push(("type", list.join(",")));
        }
        if let Some(address) = involved {
            query.push(("involved", address.to_string()));
        }
        push_page_args(&mut query, first, after);
        self.get("events", &query).await
    }

    /// Walk [`events`](Self::events) to exhaustion, collecting all items
    /// across pages.
    pub async fn paginate_events(
        &self,
        types: &[EventType],
        involved: Option<Addr>,
        page_size: Option<u32>,
    ) -> anyhow::Result<Vec<Event>> {
        collect_pages(|after| self.events(types, involved, page_size, after)).await
    }

    /// `GET /events/contract` — one page of the contract events of one
    /// emitting `contract`, optionally narrowed to a participant `user` and/or
    /// a set of event `names` (an empty slice omits the argument).
    /// Newest-first.
    pub async fn contract_events(
        &self,
        contract: Addr,
        user: Option<Addr>,
        names: &[&str],
        first: Option<u32>,
        after: Option<String>,
    ) -> anyhow::Result<Page<Event>> {
        let mut query = vec![("contract", contract.to_string())];
        push_event_filters(&mut query, user, names);
        push_page_args(&mut query, first, after);
        self.get("events/contract", &query).await
    }

    /// Walk [`contract_events`](Self::contract_events) to exhaustion,
    /// collecting all items across pages.
    pub async fn paginate_contract_events(
        &self,
        contract: Addr,
        user: Option<Addr>,
        names: &[&str],
        page_size: Option<u32>,
    ) -> anyhow::Result<Vec<Event>> {
        collect_pages(|after| self.contract_events(contract, user, names, page_size, after)).await
    }

    /// `GET /events/perps` — shortcut for
    /// [`contract_events`](Self::contract_events) with the contract pre-bound
    /// server-side to the deployment's perps address. Same `user` / `names`
    /// narrowing.
    pub async fn perps_events(
        &self,
        user: Option<Addr>,
        names: &[&str],
        first: Option<u32>,
        after: Option<String>,
    ) -> anyhow::Result<Page<Event>> {
        let mut query = Vec::new();
        push_event_filters(&mut query, user, names);
        push_page_args(&mut query, first, after);
        self.get("events/perps", &query).await
    }

    /// Walk [`perps_events`](Self::perps_events) to exhaustion, collecting all
    /// items across pages.
    pub async fn paginate_perps_events(
        &self,
        user: Option<Addr>,
        names: &[&str],
        page_size: Option<u32>,
    ) -> anyhow::Result<Vec<Event>> {
        collect_pages(|after| self.perps_events(user, names, page_size, after)).await
    }

    /// The block at `height`, or the frontier block when `None` — erroring on
    /// absence, for the [`BlockClient`] impl.
    async fn block_data(&self, height: Option<u64>) -> anyhow::Result<BlockData> {
        match height {
            Some(height) => self
                .block(height)
                .await?
                .ok_or_else(|| anyhow!("block {height} is not held by the archive")),
            None => self
                .latest_block()
                .await?
                .ok_or_else(|| anyhow!("the archive holds no blocks yet")),
        }
    }
}

/// The archive can back anything that reads blocks through the [`BlockClient`]
/// trait; `None` resolves to the archive's contiguous frontier rather than the
/// chain tip.
#[async_trait]
impl BlockClient for ArchiveClient {
    type Error = anyhow::Error;

    async fn query_block(&self, height: Option<u64>) -> Result<Block, Self::Error> {
        Ok(self.block_data(height).await?.block)
    }

    async fn query_block_outcome(&self, height: Option<u64>) -> Result<BlockOutcome, Self::Error> {
        Ok(self.block_data(height).await?.outcome)
    }
}

// ---- query-string helpers ----

/// An argument enum's wire spelling, taken from its serde serialization — so
/// the query string can never drift from the spelling the server parses.
fn snake<T>(value: &T) -> anyhow::Result<String>
where
    T: Serialize,
{
    match serde_json::to_value(value)? {
        serde_json::Value::String(text) => Ok(text),
        other => bail!("expected a string serialization, got: {other}"),
    }
}

/// Append the shared `first` / `after` pagination arguments.
fn push_page_args(query: &mut Vec<(&str, String)>, first: Option<u32>, after: Option<String>) {
    if let Some(first) = first {
        query.push(("first", first.to_string()));
    }
    if let Some(after) = after {
        query.push(("after", after));
    }
}

/// Append the `user` / `names` narrowing shared by the contract-event feeds.
fn push_event_filters(query: &mut Vec<(&str, String)>, user: Option<Addr>, names: &[&str]) {
    if let Some(user) = user {
        query.push(("user", user.to_string()));
    }
    if !names.is_empty() {
        query.push(("names", names.join(",")));
    }
}

/// Walk a keyset-paginated feed to exhaustion: fetch with `after = None`, roll
/// each page's `endCursor` back in until `hasNextPage` is false, and collect
/// all items.
async fn collect_pages<N, F, Fut>(fetch: F) -> anyhow::Result<Vec<N>>
where
    F: Fn(Option<String>) -> Fut,
    Fut: Future<Output = anyhow::Result<Page<N>>>,
{
    let mut all_items = Vec::new();
    let mut after = None;

    loop {
        let page = fetch(after.clone()).await?;
        all_items.extend(page.items);

        if !page.page_info.has_next_page {
            break;
        }
        after = page.page_info.end_cursor;
        if after.is_none() {
            break;
        }
    }

    Ok(all_items)
}
