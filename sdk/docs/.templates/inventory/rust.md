# Rust SDK Inventory

Single crate: `dango-sdk` at `dango/sdk/`. `lib.rs` glob-re-exports
`client::*`, `keystore::*`, `secret::*`, `signer::*`, `subscription::*`, and
the whole `dango_indexer_graphql_types` crate. Everything below is reachable from
the crate root unqualified.

## Summary
- Clients/structs with public methods: 5 (`HttpClient`, `WsClient`, `Session`, `SingleSigner`, `Keystore`)
- Public methods (total): 27 (4 on `HttpClient` constructors/pagination + 6 macro-generated `paginate_*` + 8 trait impls on `HttpClient` + 2 on `WsClient` ctors + 2 on `WsClient`/`Session` `subscribe`/`connect` + 11 on `SingleSigner` + 2 on `Keystore`)
- Subscription types: 14 generated `Subscribe*` query structs + `SubscriptionStream<T>` alias + `Session` + `WsClient` + `SubscriptionVariables` trait
- Standalone functions: 0 (all functionality hangs off types)
- Errors: 1 (`WsError`) — `anyhow::Error` is the catch-all everywhere else

## Public surface

### `HttpClient`
GraphQL+REST client over `reqwest`; implements grug's `QueryClient`, `BlockClient`, `BroadcastClient`, `SearchTxClient` traits. Source: `dango/sdk/src/client.rs`

Constructors:
- `pub fn new<U>(url: U) -> Result<Self, anyhow::Error> where U: IntoUrl` — wraps a `reqwest::Client` around the given Dango HTTP endpoint

Pagination helpers (own methods):
- `pub async fn paginate_all<V, N, BuildVariables, ExtractPage>(&self, first: Option<i64>, last: Option<i64>, build_variables: BuildVariables, extract_page: ExtractPage) -> Result<Vec<N>, anyhow::Error>` — generic forward/backward cursor pagination loop; closures pick the connection field

Macro-generated per-query paginators (each: `pub async fn(&self, page_size: i64, variables: <module>::Variables) -> Result<Vec<<module>::<Node>>, anyhow::Error>`):
- `paginate_accounts` → `accounts::AccountsAccountsNodes`
- `paginate_transfers` → `transfers::TransfersTransfersNodes`
- `paginate_transactions` → `transactions::TransactionsTransactionsNodes`
- `paginate_blocks` → `blocks::BlocksBlocksNodes`
- `paginate_events` → `events::EventsEventsNodes`
- `paginate_messages` → `messages::MessagesMessagesNodes`

Trait methods reachable on `HttpClient` (via `#[async_trait]` impls of grug traits — these are the canonical "Client actions"):
- `impl QueryClient` (`type Error = anyhow::Error; type Proof = dango_primitives::Proof;`)
  - `async fn query_app(&self, query: Query) -> Result<QueryResponse, Self::Error>`
  - `async fn simulate(&self, tx: UnsignedTx) -> Result<TxOutcome, Self::Error>`
- `impl BlockClient` (`type Error = anyhow::Error;`)
  - `async fn query_block(&self, height: Option<u64>) -> Result<Block, Self::Error>` — REST `block/info[/{height}]`
  - `async fn query_block_outcome(&self, height: Option<u64>) -> Result<BlockOutcome, Self::Error>` — REST `block/result[/{height}]`
- `impl BroadcastClient` (`type Error = anyhow::Error;`)
  - `async fn broadcast_tx(&self, tx: Tx) -> Result<BroadcastTxOutcome, Self::Error>`
- `impl SearchTxClient` (`type Error = anyhow::Error;`)
  - `async fn search_tx(&self, hash: Hash256) -> Result<SearchTxOutcome, Self::Error>`

The grug `QueryClientExt` blanket trait additionally hands `HttpClient`
convenience methods such as `query_app_config`, `query_wasm_smart`,
`query_balance`, etc. — those are documented in `grug`, not in `dango-sdk`,
but they appear in the SDK's example code (`signer.rs`).

### `WsClient` / `Session` / subscription helpers
GraphQL-over-WebSocket client speaking `graphql-transport-ws` on top of `tokio-tungstenite`, with a fixed-cadence (15 s) keepalive ping. Source: `dango/sdk/src/subscription.rs`

Types:
- `pub struct WsClient { url: Url }` — `Debug + Clone`; cheap value, no live connection
- `pub struct Session { inner: Arc<SessionInner> }` — `Debug + Clone`; cloneable handle to a live WS session, drops the connection when last clone + every derived stream is gone
- `pub type SubscriptionStream<T> = Pin<Box<dyn Stream<Item = Result<Response<T>, WsError>> + Send>>` — `Send` boxed stream (no `Sync` requirement)
- `pub trait SubscriptionVariables: Variables` — extension trait pre-implemented for the 13 `dango_indexer_graphql_types::subscribe_*::Variables` types so users can call `vars.subscribe(&client)` instead of `client.subscribe::<Q>(vars)`

`WsClient` methods:
- `pub fn new(url: impl Into<String>) -> Result<Self, anyhow::Error>` — accepts `ws://` / `wss://`, errors on any other scheme
- `pub fn from_http_url(url: impl Into<String>) -> Result<Self, anyhow::Error>` — converts `http://`→`ws://` and `https://`→`wss://`, passes `ws[s]://` through
- `pub async fn connect(&self) -> Result<Session, anyhow::Error>` — performs the `connection_init` / `connection_ack` handshake and spawns the background `run_session` task
- `pub async fn subscribe<Q>(&self, variables: Q::Variables) -> Result<SubscriptionStream<Q::ResponseData>, anyhow::Error>` where `Q: GraphQLQuery + Unpin + Send + Sync + 'static`, `Q::Variables: Unpin + Send + Sync + 'static`, `Q::ResponseData: DeserializeOwned + Unpin + Send + Sync + 'static` — convenience: connect, subscribe once, drop session when stream ends

`Session` methods:
- `pub async fn subscribe<Q>(&self, variables: Q::Variables) -> Result<SubscriptionStream<Q::ResponseData>, anyhow::Error>` (same bounds as above) — multiplex a new subscription onto the existing connection

`SubscriptionVariables` (provided method):
- `fn subscribe(self, client: &WsClient) -> impl Future<Output = Result<SubscriptionStream<<Self::Query as GraphQLQuery>::ResponseData>, anyhow::Error>> + Send` — sugar over `WsClient::subscribe::<Self::Query>(self)`

Pre-implemented for these `dango_indexer_graphql_types` `Variables` types: `subscribe_block`, `subscribe_accounts`, `subscribe_transfers`, `subscribe_transactions`, `subscribe_messages`, `subscribe_events`, `subscribe_event_by_addresses`, `subscribe_candles`, `subscribe_perps_candles`, `subscribe_trades`, `subscribe_perps_trades`, `subscribe_query_app`, `subscribe_query_status`. (13 impls — note that `SubscribeBlock`/`SubscribeAccounts`/etc. are the corresponding `GraphQLQuery` types.)

### `SingleSigner` / signing surface
Type-state builder for Dango's single-signature accounts; `S: Secret`, plus two phantom-state generics for `UserIndex` (defined/undefined) and `Nonce` (defined/undefined). Source: `dango/sdk/src/signer.rs`

Constants:
- `pub const DEFAULT_DERIVATION_PATH: &str = "m/44'/60'/0'/0/0"` — Ethereum coin type, used as the documentation default

Type:
- `pub struct SingleSigner<S, I = Defined<UserIndex>, N = Defined<Nonce>> where S: Secret, I: MaybeDefined<UserIndex>, N: MaybeDefined<Nonce>` with `pub address: Addr`, `pub secret: S`, `pub user_index: I`, `pub nonce: N`

Methods on every state:
- `pub async fn query_user_index<C>(&self, client: &C) -> anyhow::Result<UserIndex> where C: QueryClient, anyhow::Error: From<C::Error>` — looks up the user index from the account factory for this address
- `pub async fn query_next_nonce<C>(&self, client: &C) -> anyhow::Result<Nonce> where C: QueryClient, anyhow::Error: From<C::Error>` — returns last-seen-nonce + 1, or 0 if none

Constructors / state transitions:
- `SingleSigner<S, Undefined<UserIndex>, Undefined<Nonce>>::new(address: Addr, secret: S) -> Self` — bare constructor; index and nonce must be filled in before signing
- `SingleSigner<S, Defined<UserIndex>, Undefined<Nonce>>::new_first_address_available<C>(client: &C, secret: S, cfg: Option<&AppConfig>) -> anyhow::Result<Self>` (async) — discovers the first account+user-index associated with `secret.key_hash()` via `QueryForgotUsernameRequest`
- `pub fn with_user_index(self, user_index: UserIndex) -> SingleSigner<S, Defined<UserIndex>, N>` (consumes `self`)
- `pub async fn with_query_user_index<C>(self, client: &C) -> anyhow::Result<SingleSigner<S, Defined<UserIndex>, N>>` (consumes `self`) — same as above but queried
- `pub fn with_nonce(self, nonce: Nonce) -> SingleSigner<S, I, Defined<Nonce>>` (consumes `self`)
- `pub async fn with_query_nonce<C>(self, client: &C) -> anyhow::Result<SingleSigner<S, I, Defined<Nonce>>>` (consumes `self`)

Accessors (only available on the `Defined` variant):
- `pub fn user_index(&self) -> UserIndex` (requires `I = Defined<UserIndex>`)
- `pub fn nonce(&self) -> Nonce` (requires `N = Defined<Nonce>`)

Trait impls:
- `impl<S, I, N> Addressable for SingleSigner<S, I, N>` — `fn address(&self) -> Addr`
- `impl<S> Signer for SingleSigner<S>` (i.e. fully defined index + nonce):
  - `fn unsigned_transaction(&self, msgs: NonEmpty<Vec<Message>>, chain_id: &str) -> StdResult<UnsignedTx>`
  - `fn sign_transaction(&mut self, msgs: NonEmpty<Vec<Message>>, chain_id: &str, gas_limit: u64) -> StdResult<Tx>` — auto-increments the in-memory nonce on success
- `#[async_trait] impl<S> SequencedSigner for SingleSigner<S, Defined<Nonce>> where S: Secret + Send + Sync`:
  - `async fn query_nonce<C>(&self, client: &C) -> anyhow::Result<Nonce>`
  - `async fn update_nonce<C>(&mut self, client: &C) -> anyhow::Result<()>`

### `Secret` trait + concrete secrets
Source: `dango/sdk/src/secret.rs`

Trait `pub trait Secret: Sized` with associated types `Private`, `Public`, `Signature`:
- `fn new_random() -> Self` (provided; uses `OsRng`)
- `fn from_rng(rng: &mut impl CryptoRngCore) -> Self`
- `fn from_bytes(bytes: Self::Private) -> anyhow::Result<Self>`
- `fn from_mnemonic(mnemonic: &Mnemonic, coin_type: usize) -> anyhow::Result<Self>` — BIP-44 at `m/44'/{coin_type}'/0'/0/0`, empty seed password
- `fn private_key(&self) -> Self::Private`
- `fn public_key(&self) -> Self::Public`
- `fn key(&self) -> Key` (returns `dango_types::auth::Key`)
- `fn key_hash(&self) -> Hash256`
- `fn sign_transaction(&self, sign_doc: SignDoc) -> anyhow::Result<Signature>` (returns `dango_types::auth::Signature`)

Implementors:
- `pub struct Secp256k1` (`Debug + Clone`) — `Private = [u8; 32]`, `Public = [u8; 33]`, `Signature = [u8; 64]`; raw secp256k1 over SHA-256 of `SignDoc`
- `pub struct Eip712 { inner: Secp256k1, pub address: dango_eth_utils::Address }` (`Debug + Clone`) — same private/public byte sizes as `Secp256k1`, `Signature = [u8; 65]` (sig + recovery id); signs the SignDoc as EIP-712 typed data with domain `{ name: "dango", chain_id: EIP155_CHAIN_ID, verifying_contract: <sender as U160> }`; `key()` returns `Key::Ethereum(address)`; `key_hash()` is `sha256(addr.to_string())` (string form, not raw bytes)
- `impl From<Secp256k1> for Eip712` — derives Ethereum address from the verifying key

(`// TODO: Secp256r1 secret.` comment exists in source — Secp256r1 is **not** implemented.)

### `Keystore`
Encrypted-on-disk container for a 32-byte private key (AES-256-GCM + PBKDF2-HMAC-SHA256, 600 000 iters, 16-byte salt, 12-byte nonce). Source: `dango/sdk/src/keystore.rs`

Type:
- `pub struct Keystore` (derives `dango_primitives::derive(Serde)`) with public fields `pk: ByteArray<33>`, `salt: ByteArray<16>`, `nonce: ByteArray<12>`, `ciphertext: Binary`

Static methods (no `impl Secret` here — `Keystore` is just a file format):
- `pub fn from_file<F, P>(filename: F, password: P) -> anyhow::Result<[u8; 32]> where F: AsRef<Path>, P: AsRef<[u8]>` — reads the file, decrypts, returns the raw 32-byte private key (caller wraps it in `Secp256k1::from_bytes` / `Eip712::from_bytes`)
- `pub fn write_to_file<S, F, P>(secret: &S, filename: F, password: P) -> anyhow::Result<Self> where S: Secret, S::Private: AsRef<[u8]>, S::Public: Into<ByteArray<33>>, F: AsRef<Path>, P: AsRef<[u8]>` — encrypts `secret.private_key()` and writes a pretty-printed JSON file; returns the in-memory `Keystore`

### Re-exported `dango_indexer_graphql_types` surface
`pub use dango_indexer_graphql_types::*` re-exports (source: `dango/indexer/graphql-types/src/lib.rs`):

- `pub trait Variables { type Query: GraphQLQuery<Variables = Self>; }` — links a `*::Variables` struct to its parent `GraphQLQuery` type; implemented by macro for every query and subscription module
- `pub struct PageInfo { pub start_cursor, pub end_cursor: Option<String>, pub has_next_page, pub has_previous_page: bool }` (`Debug + Clone + Default + Serialize + Deserialize`)
- Generated query types (each a `pub struct $Name;` implementing `graphql_client::GraphQLQuery`, with an accompanying snake-case module containing `Variables`, `ResponseData`, and nested `*Nodes` records): `QueryApp`, `Simulate`, `BroadcastTxSync`, `SearchTx`, `Block`, `Blocks`, `Transactions`, `Messages`, `Events`, `Transfers`, `Accounts`, `User`, `Users`, `Candles`, `PerpsCandles`, `PerpsEvents`, `Trades`, `PairStats`, `PairStatsPartial`, `AllPairStats`, `PerpsPairStats`, `PerpsPairStatsPartial`, `AllPerpsPairStats`, `QueryStatus` — 25 query types
- Generated subscription types: `SubscribeBlock`, `SubscribeAccounts`, `SubscribeTransfers`, `SubscribeTransactions`, `SubscribeMessages`, `SubscribeEvents`, `SubscribeEventByAddresses`, `SubscribeCandles`, `SubscribePerpsCandles`, `SubscribeTrades`, `SubscribePerpsTrades`, `SubscribeQueryApp`, `SubscribeQueryStatus` — 14 subscription types
- `pub mod subscriptions` — convenience module re-exporting the snake-case subscription submodules (`subscribe_block`, `subscribe_accounts`, …)
- `Default` impls for: `candles::CandleInterval` (→ `ONE_MINUTE`), `perps_candles::CandleInterval`, `subscribe_candles::CandleInterval`, `subscribe_perps_candles::CandleInterval`, `subscribe_events::CheckValue` (→ `EQUAL`)

### Standalone functions / module-level exports
None at the crate top level. Every operation hangs off one of the types above. (The internal `error_for_status` helper in `client.rs` and `run_session` task in `subscription.rs` are not `pub`.)

### Errors
- `pub enum WsError` (`Debug + Clone + thiserror::Error`) — variants `Closed(String)`, `Transport(String)`, `Subscription(serde_json::Value)`, `Decode(String)`. Returned **only** inside subscription stream items (`Result<Response<T>, WsError>`). Source: `dango/sdk/src/subscription.rs`

Everywhere else the SDK returns `anyhow::Result<T>` / `Result<T, anyhow::Error>` and uses `anyhow!` / `bail!` / `ensure!` to construct errors. The grug traits (`QueryClient`, `BroadcastClient`, …) keep `type Error = anyhow::Error` for `HttpClient`. There is no public typed error enum analogous to TS's `BaseError`/`HttpRequestError`.

### Feature-gated items
- The `tracing` feature only adds `tracing::debug!` log calls inside `HttpClient::post_graphql` (GraphQL request/response logging) and `WsClient::connect` / `Session::subscribe` (handshake + subscribe-id logs). **No public symbols are gated behind `tracing`.** Enabling/disabling the feature does not change the API surface.

## Excluded items (do not document)
- `error_for_status` (`client.rs:412`) — private helper, not `pub`
- `run_session` (`subscription.rs:374`) — private background task driver
- `ClientMessage`, `ServerMessage`, `Command`, `SubscriptionStreamInner`, `WsStream`, `ResponseSender`, `ResponseReceiver`, `SessionInner`, `KEEP_ALIVE_INTERVAL` — private types/aliases/consts in `subscription.rs`
- `SECP256K1_COMPRESSED_PUBKEY_LEN`, `PBKDF2_ITERATIONS`, `PBKDF2_SALT_LEN`, `PBKDF2_KEY_LEN`, `AES256GCM_NONCE_LEN` (`keystore.rs:12-16`) — private consts; the field types use the numeric literals directly via `ByteArray<N>`
- `examples/subscribe_order_filled.rs`, `examples/trade_history_csv.rs` — examples, not crate API
- `dango/sdk/cli/` — separate `dango-sdk-cli` binary crate, out of scope for the library inventory
- `tests/*.rs` — integration tests, not API
- `dango_auth::EIP155_CHAIN_ID`, `dango_types::auth::*`, `dango_primitives::*` — used in signatures but owned by other crates; document those crates in their own context if needed (the docs site only exposes `dango-sdk`'s own re-exports)

## Verification TODOs (drafters must confirm before writing)
- `SubscriptionStream<T>` is `Send` but the trait bound says `dyn Stream<...> + Send` (no `Sync`). Confirm whether `tokio::spawn(stream.for_each(...))` works in practice (it should, since `Send` is sufficient for `tokio::spawn`'s future). Also note streams are **`Pin<Box<dyn ...>>`** — drafters writing `let mut stream = ...; while let Some(x) = stream.next().await {}` do not need to manually `Box::pin`.
- `SingleSigner::sign_transaction` mutates `self.nonce` via `*self.nonce.inner_mut() += 1` even on the `Signer` trait method (takes `&mut self`). Document the side effect.
- `Eip712::key_hash()` uses `Addr::from(self.address).to_string().as_bytes().sha2_256()` — confirm whether docs should call out that this is `sha256(string_repr)` not `sha256(raw_bytes)` (it differs from `Secp256k1::key_hash` which hashes the compressed pubkey directly).
- `SingleSigner::new_first_address_available` uses `limit: Some(1)` against `QueryForgotUsernameRequest` — drafters should verify the semantics ("first user index for this key", regardless of forgotten-username status) and pick the right Concept page wording.
- `WsClient::subscribe` opens a **dedicated** connection per call; `Session::subscribe` reuses one. Make sure the Subscriptions concept page contrasts the two clearly.
- `HttpClient::paginate_all` requires **exactly one** of `first` / `last` to be `Some` (errors otherwise). Note this in its Action page.
- `Keystore::from_file` returns the raw `[u8; 32]` private key, not a `Secret`. Drafters must show users how to wrap it (`Secp256k1::from_bytes(bytes)?`).
- The grug `QueryClientExt` blanket trait gives `HttpClient` many convenience methods (`query_app_config`, `query_wasm_smart`, `query_balance`, …) that are **not** re-exported by `dango-sdk` but are used in the SDK's own examples. Decide whether to document them in the `Client` page or link out to grug.
- `dango_indexer_graphql_types` generates **dozens** of nested `*Nodes`, `*Edges`, `*PageInfo` structs per query. Type pages must pick a small set of user-facing types (`PageInfo`, `*::Variables`, `*::ResponseData`) and not try to document every generated record.
- Confirm whether the `subscriptions` submodule re-export (`dango_indexer_graphql_types::subscriptions`) shows up at the `dango-sdk` crate root via the glob re-export — it should, but verify in `cargo doc`.
