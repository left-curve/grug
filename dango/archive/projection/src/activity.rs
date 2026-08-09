//! The activity projection — the foundational projection of the archive.
//! For every block it writes three Postgres tables that together
//! answer, for any account address, *"what did it do and what touched it"*:
//!
//! - [`entity::transactions`] — one row per executed unit (a tx or a cronjob);
//! - [`entity::events`] — the merged event log + participation index: one row
//!   per (event × participant address), with an empty-address row for kept
//!   events that have no participant;
//! - [`entity::event_data`] — the event payload, split out and kept only for
//!   priority types.
//!
//! A single [`process`](ActivityProjection::process) flattens the block's
//! events once and stages all three tables into one [`Ctx`], so they commit
//! atomically with the projection's cursor. Only **committed** events are
//! indexed — a reverted or failed event never took effect on-chain, so it is
//! not activity (the failed unit itself stays visible through its
//! `transactions` row, `success = false`). Every key is the deterministic
//! position (+ address) and every insert is `ON CONFLICT DO NOTHING`, so a
//! post-crash replay of a block is a no-op. See `DESIGN.md` in this folder.

mod entity;
mod event_type;
mod http;
mod idens;
mod migrations;

pub use event_type::EventType;
#[cfg(feature = "tracing")]
use tracing::instrument;
use {
    crate::{Ctx, Projection, WhiteOrBlackList},
    actix_web::Scope,
    async_trait::async_trait,
    dango_archive_types::{AnyResult, BlockData},
    dango_primitives::{
        Addr, EventId, Extractable, FlatCategory, FlatCommitmentStatus, FlatEvent, FlatEventInfo,
        flatten_commitment_status, flatten_tx_events,
    },
    entity::{event_data, events, transactions},
    event_type::{contract_event_name, event_contract},
    sea_orm::{ActiveValue::Set, EntityTrait, sea_query::OnConflict},
    sea_orm_migration::MigrationTrait,
    std::collections::HashSet,
};

/// Stable projection id keying the cursor row. Bumping it forces a full
/// re-backfill.
const PROJECTION_ID: &str = "activity";

/// zstd level for the `event_data.data` column. 3 is zstd's own default — a
/// good ratio/speed trade-off for the borsh-encoded event payloads.
const ZSTD_LEVEL: i32 = 3;

/// Rows per `INSERT` statement in [`Rows::write`]. One statement binds
/// `rows × columns` parameters, and the engines cap that (65_535 on Postgres,
/// 32_766 on SQLite) — a pathological block (an airdrop fanning out thousands
/// of transfers) would blow the cap in one unchunked insert, and since the
/// replay re-fails identically, crash-loop the projection on that block
/// forever. 4_000 rows × 8 columns (the widest table here) stays under both
/// caps.
const INSERT_CHUNK: usize = 4_000;

// ---- configuration ----

/// Write-time knobs for the activity projection. Applied as each block is
/// processed, so changing them is **not** retroactive (see `DESIGN.md` §
/// Configuration): re-populating already-written rows needs a re-backfill.
#[derive(Clone, Debug)]
pub struct ActivityConfig {
    /// Which event types are **kept** (indexed at all). An event whose type the
    /// filter rejects gets no `events` row and no payload. Default: a blacklist
    /// of the address-less system noise (guest/withhold/authenticate/finalize/…)
    /// that dominates the raw stream and is not user-facing activity.
    pub event_type_filter: WhiteOrBlackList<EventType>,
    /// Which event types have their payload (`zstd(borsh(event))`) stored in
    /// `event_data`; others are hydrated from the raw block on demand. Default:
    /// a whitelist of the priority types.
    pub event_data_filter: WhiteOrBlackList<EventType>,
    /// Which event types have their participants extracted into the `events`
    /// rows (one row per participant); other kept types get a single
    /// empty-address row. Default: a whitelist of the priority types.
    pub involvement_filter: WhiteOrBlackList<EventType>,
    /// Addresses excluded from participation at write time — the deployment's
    /// system contracts, merged in by the cli from the node's `app_config`.
    pub involvement_blacklist: HashSet<Addr>,
    /// The perps contract's address — the anchor of the `/events/perps`
    /// shortcut route (the contract-events feeds with the contract argument
    /// pre-bound). Injected by the cli from the node's `app_config`
    /// (`addresses.perps`); `None` (the default) leaves the route unmounted.
    /// Read-time only — it affects no written row.
    pub perps_contract: Option<Addr>,
}

impl Default for ActivityConfig {
    fn default() -> Self {
        let priority = HashSet::from([EventType::Transfer, EventType::ContractEvent]);
        Self {
            // The pure plumbing: emitted on every call/tx, never carries a
            // meaningful participant. Measured ~81% of the raw mainnet stream.
            event_type_filter: WhiteOrBlackList::Blacklist(HashSet::from([
                EventType::Guest,
                EventType::Withhold,
                EventType::Authenticate,
                EventType::Finalize,
                EventType::Backrun,
                EventType::Reply,
            ])),
            event_data_filter: WhiteOrBlackList::Whitelist(priority.clone()),
            involvement_filter: WhiteOrBlackList::Whitelist(priority),
            involvement_blacklist: HashSet::new(),
            perps_contract: None,
        }
    }
}

// ---- projection ----

/// See the [module docs](self).
#[derive(Clone, Debug, Default)]
pub struct ActivityProjection {
    config: ActivityConfig,
}

impl ActivityProjection {
    #[must_use]
    pub fn new(config: ActivityConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Projection for ActivityProjection {
    fn id(&self) -> &'static str {
        PROJECTION_ID
    }

    fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
        migrations::migrations()
    }

    fn services(&self) -> Vec<Scope> {
        http::scopes(self.config.perps_contract)
    }

    fn api_doc(&self) -> Option<utoipa::openapi::OpenApi> {
        Some(http::api_doc(self.config.perps_contract.is_some()))
    }

    #[cfg_attr(feature = "tracing", instrument(skip_all, fields(height = block.block.info.height)))]
    async fn process(&self, ctx: &mut Ctx, block: &BlockData) -> AnyResult<()> {
        let block_height = block.block.info.height;
        let height = block_height as i64;
        let timestamp = block.block.info.timestamp.into_nanos() as i64;

        let mut rows = Rows::default();

        // Each unit is flattened on its own `EventId`, so `event_index` is
        // 0-based within the unit; `(block_height, category, category_index,
        // event_index)` is the canonical event position, unique because
        // `category_index` separates the units.

        // ---- cron units ----
        for (cron_idx, cron_outcome) in block.outcome.cron_outcomes.iter().enumerate() {
            rows.transactions.push(transactions::ActiveModel {
                block_height: Set(height),
                idx: Set(cron_idx as i32),
                kind: Set(FlatCategory::Cron as i16),
                hash: Set(None),
                sender: Set(None),
                success: Set(cron_outcome.cron_event.maybe_error().is_none()),
                timestamp: Set(timestamp),
            });

            // A cronjob is a single commitment group; `flatten_unit` numbers
            // its events exactly as the read path will (one source of truth).
            // Cron has no sender, so no `transactions.sender` row feeds query
            // 1's sender side — only its participants.
            let flat = flatten_unit(block, FlatCategory::Cron as i16, cron_idx);
            self.push_events(&mut rows, &flat, height)?;
        }

        // ---- tx units ----
        for (tx_idx, ((tx, tx_hash), tx_outcome)) in block
            .block
            .txs
            .iter()
            .zip(block.outcome.tx_outcomes.iter())
            .enumerate()
        {
            rows.transactions.push(transactions::ActiveModel {
                block_height: Set(height),
                idx: Set(tx_idx as i32),
                kind: Set(FlatCategory::Tx as i16),
                hash: Set(Some(tx_hash.as_ref().to_vec())),
                sender: Set(Some(tx.sender.as_ref().to_vec())),
                success: Set(tx_outcome.result.is_ok()),
                timestamp: Set(timestamp),
            });

            // `flatten_unit` flattens the four commitment groups (withhold,
            // authenticate, msgs + backrun, finalize) on a fresh per-tx id —
            // the same numbering the read path reuses.
            let flat = flatten_unit(block, FlatCategory::Tx as i16, tx_idx);
            self.push_events(&mut rows, &flat, height)?;
        }

        #[cfg(feature = "metrics")]
        {
            metrics::counter!(crate::metrics::ACTIVITY_TRANSACTIONS)
                .increment(rows.transactions.len() as u64);
            metrics::counter!(crate::metrics::ACTIVITY_EVENTS).increment(rows.events.len() as u64);
            metrics::counter!(crate::metrics::ACTIVITY_EVENT_DATA)
                .increment(rows.event_data.len() as u64);
        }

        rows.write(ctx).await
    }
}

impl ActivityProjection {
    /// Stage every flattened event of one unit.
    fn push_events(&self, rows: &mut Rows, flat: &[FlatEventInfo], height: i64) -> AnyResult<()> {
        for (position, info) in flat.iter().enumerate() {
            // The read path (`Event::data`'s non-priority fallback) re-flattens
            // the unit and finds an event by its `event_index`; the write path
            // stores that same `id.event_index`. The numbering is dense `0..n`
            // matching the flattened Vec position — pin that here so a flatten
            // change can't silently desync the two sides.
            debug_assert_eq!(
                info.id.event_index as usize, position,
                "flattened event_index must equal its position within the unit"
            );
            self.push_event(rows, info, height)?;
        }

        Ok(())
    }

    /// Stage one flattened event into the merged `events` table: one row per
    /// participant (or a single empty-address row if it has none), plus the
    /// payload into `event_data` for priority types. Non-committed events and
    /// blacklisted types are dropped wholesale.
    fn push_event(&self, rows: &mut Rows, info: &FlatEventInfo, height: i64) -> AnyResult<()> {
        // Only **committed** events are indexed: a reverted or failed event (a
        // transfer inside a tx that later failed, say) never took effect
        // on-chain, so surfacing it in the feeds would show activity that did
        // not happen — the same rule the in-process indexer applies when it
        // reads transfers (`Committed` only). The failed unit itself stays
        // visible through its `transactions` row (`success = false`); only its
        // side-effect events are dropped.
        if info.commitment_status != FlatCommitmentStatus::Committed {
            return Ok(());
        }

        let event = &info.event;
        let event_type = EventType::from(event);

        if !self.config.event_type_filter.allows(&event_type) {
            return Ok(());
        }

        let category = info.id.category as i16;
        let category_index = info.id.category_index as i32;
        let event_index = info.id.event_index as i32;

        let contract = event_contract(event).map(|a| a.as_ref().to_vec());
        let contract_event_name = contract_event_name(event);

        // Payload into the side-table, only for priority types.
        if self.config.event_data_filter.allows(&event_type) {
            rows.event_data.push(event_data::ActiveModel {
                block_height: Set(height),
                category: Set(category),
                category_index: Set(category_index),
                event_index: Set(event_index),
                data: Set(compress_event(event)?),
            });
        }

        // Participants for the configured involvement types, minus the
        // blacklist. An event with no participant still gets one row, keyed by
        // the empty address, so the attribute feeds never lose it.
        let mut addresses: Vec<Vec<u8>> = if self.config.involvement_filter.allows(&event_type) {
            let mut set = HashSet::new();
            event.extract_addresses(&mut set);
            set.into_iter()
                .filter(|a| !self.config.involvement_blacklist.contains(a))
                .map(|a| a.as_ref().to_vec())
                .collect()
        } else {
            Vec::new()
        };
        if addresses.is_empty() {
            addresses.push(Vec::new()); // the empty-address marker
        }

        for address in addresses {
            rows.events.push(events::ActiveModel {
                address: Set(address),
                block_height: Set(height),
                category: Set(category),
                category_index: Set(category_index),
                event_index: Set(event_index),
                event_type: Set(event_type.code()),
                contract: Set(contract.clone()),
                contract_event_name: Set(contract_event_name.clone()),
            });
        }

        Ok(())
    }
}

// ---- staged writes ----

/// One block's staged rows, flushed together in [`Rows::write`].
#[derive(Default)]
struct Rows {
    transactions: Vec<transactions::ActiveModel>,
    events: Vec<events::ActiveModel>,
    event_data: Vec<event_data::ActiveModel>,
}

impl Rows {
    async fn write(self, ctx: &mut Ctx) -> AnyResult<()> {
        let txn = ctx.pg();

        for chunk in chunked(self.transactions) {
            transactions::Entity::insert_many(chunk)
                .on_conflict(
                    OnConflict::columns([
                        transactions::Column::BlockHeight,
                        transactions::Column::Idx,
                        transactions::Column::Kind,
                    ])
                    .do_nothing()
                    .to_owned(),
                )
                .exec_without_returning(txn)
                .await?;
        }

        for chunk in chunked(self.events) {
            events::Entity::insert_many(chunk)
                .on_conflict(
                    OnConflict::columns([
                        events::Column::Address,
                        events::Column::BlockHeight,
                        events::Column::Category,
                        events::Column::CategoryIndex,
                        events::Column::EventIndex,
                    ])
                    .do_nothing()
                    .to_owned(),
                )
                .exec_without_returning(txn)
                .await?;
        }

        for chunk in chunked(self.event_data) {
            event_data::Entity::insert_many(chunk)
                .on_conflict(
                    OnConflict::columns([
                        event_data::Column::BlockHeight,
                        event_data::Column::Category,
                        event_data::Column::CategoryIndex,
                        event_data::Column::EventIndex,
                    ])
                    .do_nothing()
                    .to_owned(),
                )
                .exec_without_returning(txn)
                .await?;
        }

        Ok(())
    }
}

/// Split staged rows into [`INSERT_CHUNK`]-sized batches, preserving order.
/// Empty input yields no batches (sea-orm rejects an empty `insert_many`), so
/// callers need no emptiness guard.
fn chunked<M>(rows: Vec<M>) -> impl Iterator<Item = Vec<M>> {
    let mut rows = rows.into_iter().peekable();
    std::iter::from_fn(move || {
        rows.peek()?;
        Some(rows.by_ref().take(INSERT_CHUNK).collect())
    })
}

/// `zstd(borsh(event))` — the stored form of a priority event's payload.
pub(crate) fn compress_event(event: &FlatEvent) -> AnyResult<Vec<u8>> {
    let borshed = borsh::to_vec(event)?;
    let compressed = zstd::encode_all(borshed.as_slice(), ZSTD_LEVEL)?;
    Ok(compressed)
}

/// Inverse of [`compress_event`]: the stored `zstd(borsh(FlatEvent))` back to
/// the event. The read path uses it to hydrate a priority event's payload from
/// the `event_data` blob the feed join carried.
pub(crate) fn decompress_event(data: &[u8]) -> AnyResult<FlatEvent> {
    let borshed = zstd::decode_all(data)?;
    Ok(borsh::from_slice(&borshed)?)
}

/// Flatten a single unit of a block — the cronjob or transaction at
/// `(category, category_index)` — into its positioned events, numbered exactly
/// as [`process`](ActivityProjection::process) writes them. Shared by the write
/// path (staging the `events` rows) and the read path (hydrating a non-priority
/// event's payload, absent from `event_data`), so `event_index` means the same
/// on both sides. Empty when the index is out of range or the category is
/// neither cron nor tx.
pub(crate) fn flatten_unit(
    block: &BlockData,
    category: i16,
    category_index: usize,
) -> Vec<FlatEventInfo> {
    let block_height = block.block.info.height;
    if category == FlatCategory::Cron as i16 {
        let Some(cron) = block.outcome.cron_outcomes.get(category_index) else {
            return Vec::new();
        };
        let mut event_id = EventId::new(block_height, FlatCategory::Cron, category_index as u32, 0);
        flatten_commitment_status(&mut event_id, cron.cron_event.clone())
    } else if category == FlatCategory::Tx as i16 {
        let Some(tx) = block.outcome.tx_outcomes.get(category_index) else {
            return Vec::new();
        };
        flatten_tx_events(tx.events.clone(), block_height, category_index as u32)
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use {super::*, dango_primitives::FlatEvtBackrun};

    #[test]
    fn event_type_codes_are_stable() {
        // Codes are part of the on-disk schema — pin them so a reorder can't
        // silently renumber stored rows.
        assert_eq!(EventType::Configure.code(), 0);
        assert_eq!(EventType::Transfer.code(), 2);
        assert_eq!(EventType::Execute.code(), 5);
        assert_eq!(EventType::ContractEvent.code(), 14);

        // `from_code` is the exact inverse used to surface `event_type` in the
        // read API, and rejects out-of-range codes.
        for code in 0..=14i16 {
            assert_eq!(EventType::from_code(code).map(EventType::code), Some(code));
        }
        assert_eq!(EventType::from_code(15), None);
        assert_eq!(EventType::from_code(-1), None);
    }

    #[test]
    fn default_config_prioritises_signal_and_blacklists_noise() {
        let cfg = ActivityConfig::default();
        for ty in [EventType::Transfer, EventType::ContractEvent] {
            assert!(cfg.event_data_filter.allows(&ty));
            assert!(cfg.involvement_filter.allows(&ty));
            assert!(cfg.event_type_filter.allows(&ty));
        }
        // The plumbing is dropped; the meaningful types are kept.
        assert!(!cfg.event_type_filter.allows(&EventType::Guest));
        assert!(!cfg.event_type_filter.allows(&EventType::Withhold));
        assert!(cfg.event_type_filter.allows(&EventType::Execute));
        assert!(cfg.involvement_blacklist.is_empty());
    }

    #[test]
    fn non_committed_events_are_dropped() {
        use dango_primitives::{FlatEventStatus, FlatEvtTransfer};

        let projection = ActivityProjection::default();
        let info = |status| FlatEventInfo {
            id: EventId::new(1, FlatCategory::Tx, 0, 0),
            parent_id: EventId::new(1, FlatCategory::Tx, 0, 0),
            commitment_status: status,
            event_status: FlatEventStatus::Ok,
            event: FlatEvent::Transfer(FlatEvtTransfer {
                sender: Addr::mock(1),
                transfers: Default::default(),
            }),
        };

        // A committed transfer is indexed: an `events` row and (a priority
        // type) its `event_data` payload.
        let mut rows = Rows::default();
        projection
            .push_event(&mut rows, &info(FlatCommitmentStatus::Committed), 1)
            .unwrap();
        assert_eq!(rows.events.len(), 1);
        assert_eq!(rows.event_data.len(), 1);

        // A reverted or failed one is dropped wholesale — no `events` row (not
        // even the empty-address marker), no payload.
        for status in [FlatCommitmentStatus::Reverted, FlatCommitmentStatus::Failed] {
            let mut rows = Rows::default();
            projection.push_event(&mut rows, &info(status), 1).unwrap();
            assert!(rows.events.is_empty(), "{status:?} must not be indexed");
            assert!(
                rows.event_data.is_empty(),
                "{status:?} must carry no payload"
            );
        }
    }

    #[test]
    fn chunked_batches_rows_in_order_and_skips_empty() {
        assert_eq!(chunked(Vec::<u8>::new()).count(), 0);

        let batches: Vec<Vec<usize>> = chunked((0..INSERT_CHUNK * 2 + 1).collect()).collect();
        assert_eq!(
            batches.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![INSERT_CHUNK, INSERT_CHUNK, 1],
        );
        // Order is preserved across the chunk boundary.
        assert_eq!(batches[1][0], INSERT_CHUNK);
        assert_eq!(batches[2][0], INSERT_CHUNK * 2);
    }

    /// A pathological block can stage more rows than one `INSERT` may carry
    /// (bind-parameter caps: 65_535 on Postgres, 32_766 on SQLite).
    /// `Rows::write` must chunk — unchunked, the 5_000 × 8-column insert below
    /// already blows SQLite's cap, so this pins the chunking on a real engine
    /// end-to-end.
    #[tokio::test]
    async fn write_chunks_oversized_blocks() {
        use sea_orm::{Database, PaginatorTrait, TransactionTrait};

        let db = Database::connect("sqlite::memory:").await.unwrap();
        let manager = sea_orm_migration::SchemaManager::new(&db);
        for migration in migrations::migrations() {
            migration.up(&manager).await.unwrap();
        }

        let mut rows = Rows::default();
        for i in 0..5_000i32 {
            rows.events.push(events::ActiveModel {
                address: Set(vec![1u8; 20]),
                block_height: Set(1),
                category: Set(FlatCategory::Tx as i16),
                category_index: Set(0),
                event_index: Set(i),
                event_type: Set(EventType::Transfer.code()),
                contract: Set(None),
                contract_event_name: Set(None),
            });
        }

        let mut ctx = Ctx::new(db.begin().await.unwrap());
        rows.write(&mut ctx).await.unwrap();
        let (txn, _ch) = ctx.into_parts();
        txn.commit().await.unwrap();

        assert_eq!(events::Entity::find().count(&db).await.unwrap(), 5_000);
    }

    #[test]
    fn flat_event_maps_to_event_type_and_has_no_contract() {
        let event = FlatEvent::Backrun(FlatEvtBackrun {
            sender: Addr::mock(1),
        });
        assert_eq!(EventType::from(&event), EventType::Backrun);
        assert!(event_contract(&event).is_none());
        assert!(contract_event_name(&event).is_none());
    }

    /// Applies the three migrations on an in-memory SQLite engine, then
    /// exercises the merged table: the address+attribute access paths (gateway
    /// / order_filled involving A), the empty-address marker for an address-less
    /// event, and the `event_data` split. Validates that the schema builds
    /// (composite PK with a bytea address incl. the empty value, the partial
    /// indexes, the side-table) end-to-end on a real engine.
    #[tokio::test]
    async fn schema_builds_and_merged_queries_round_trip() {
        use sea_orm::{ColumnTrait, Database, QueryFilter, QueryOrder};

        let db = Database::connect("sqlite::memory:").await.unwrap();
        let manager = sea_orm_migration::SchemaManager::new(&db);
        for migration in migrations::migrations() {
            migration.up(&manager).await.unwrap();
        }

        let a = vec![1u8; 20];
        let gateway = vec![0xAAu8; 20];
        let perps = vec![0xBBu8; 20];
        let tx = FlatCategory::Tx as i16;
        let ce = EventType::ContractEvent.code();

        events::Entity::insert_many([
            // gateway contract-event involving A.
            events::ActiveModel {
                address: Set(a.clone()),
                block_height: Set(100),
                category: Set(tx),
                category_index: Set(0),
                event_index: Set(4),
                event_type: Set(ce),
                contract: Set(Some(gateway.clone())),
                contract_event_name: Set(Some("bridge".to_string())),
            },
            // order_filled from perps involving A.
            events::ActiveModel {
                address: Set(a.clone()),
                block_height: Set(50),
                category: Set(tx),
                category_index: Set(0),
                event_index: Set(2),
                event_type: Set(ce),
                contract: Set(Some(perps.clone())),
                contract_event_name: Set(Some("order_filled".to_string())),
            },
            // an address-less event (empty-address marker) — a non-contract
            // event (here an Execute) with no participant: only contract events
            // carry `contract`, so it has none.
            events::ActiveModel {
                address: Set(Vec::new()),
                block_height: Set(70),
                category: Set(tx),
                category_index: Set(0),
                event_index: Set(0),
                event_type: Set(EventType::Execute.code()),
                contract: Set(None),
                contract_event_name: Set(None),
            },
        ])
        .exec_without_returning(&db)
        .await
        .unwrap();

        // "gateway events involving A" — the single block-100 row.
        let gw = events::Entity::find()
            .filter(events::Column::Address.eq(a.clone()))
            .filter(events::Column::Contract.eq(gateway.clone()))
            .all(&db)
            .await
            .unwrap();
        assert_eq!(gw.len(), 1);
        assert_eq!(gw[0].block_height, 100);

        // "order_filled involving A".
        let of = events::Entity::find()
            .filter(events::Column::Address.eq(a.clone()))
            .filter(events::Column::ContractEventName.eq("order_filled".to_string()))
            .all(&db)
            .await
            .unwrap();
        assert_eq!(of.len(), 1);
        assert_eq!(of[0].block_height, 50);

        // "all events involving A, newest-first" — gateway + order_filled = 2
        // (no sentinels now; the empty-address event does not involve A).
        let all = events::Entity::find()
            .filter(events::Column::Address.eq(a.clone()))
            .order_by_desc(events::Column::BlockHeight)
            .all(&db)
            .await
            .unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].block_height, 100);

        // The address-less event is stored under the empty address, distinct
        // from every real one.
        let mock = events::Entity::find()
            .filter(events::Column::Address.eq(Vec::<u8>::new()))
            .all(&db)
            .await
            .unwrap();
        assert_eq!(mock.len(), 1);
        assert_eq!(mock[0].block_height, 70);

        // Data split: payload lives in the side-table, fetched by the shared
        // positional key.
        event_data::Entity::insert(event_data::ActiveModel {
            block_height: Set(100),
            category: Set(tx),
            category_index: Set(0),
            event_index: Set(4),
            data: Set(vec![1, 2, 3]),
        })
        .exec_without_returning(&db)
        .await
        .unwrap();
        let payload = event_data::Entity::find_by_id((100_i64, tx, 0_i32, 4_i32))
            .one(&db)
            .await
            .unwrap();
        assert_eq!(payload.unwrap().data, vec![1, 2, 3]);
    }
}
