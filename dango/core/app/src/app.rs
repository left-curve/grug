#[cfg(any(feature = "abci", feature = "tracing"))]
use dango_primitives::JsonSerExt;
#[cfg(feature = "abci")]
use dango_primitives::{HashExt, JsonDeExt};
#[cfg(all(feature = "abci", feature = "tracing"))]
use data_encoding::BASE64;
use {
    crate::{
        APP_CONFIG, AppError, AppResult, CHAIN_ID, CODES, CONFIG, Db, EventResult, GasTracker,
        Indexer, LAST_FINALIZED_BLOCK, NEXT_CRONJOBS, NEXT_UPGRADE, NaiveProposalPreparer,
        NaiveQuerier, NullIndexer, PAST_UPGRADES, ProposalPreparer, QuerierProviderImpl,
        TraceOption, Vm, catch_and_push_event, do_authenticate, do_configure, do_cron_execute,
        do_execute, do_instantiate, do_migrate, do_transfer, do_upgrade, do_upload,
        do_withhold_fee, query_app_config, query_balance, query_balances, query_code, query_codes,
        query_config, query_contract, query_contracts, query_next_upgrade, query_past_upgrades,
        query_status, query_supplies, query_supply, query_wasm_raw, query_wasm_scan,
        query_wasm_smart,
    },
    dango_primitives::{
        Addr, AuthMode, Block, BlockInfo, BlockOutcome, Buffer, CheckTxEvents, CheckTxOutcome,
        CodeStatus, CronOutcome, Duration, Event, GENESIS_SENDER, GenericResult, GenericResultExt,
        GenesisState, Hash256, Json, Message, MsgsAndBackrunEvents, Order, Permission,
        QuerierWrapper, Query, QueryResponse, Shared, StdResult, Storage, Timestamp, Tx, TxEvents,
        TxOutcome, UnsignedTx,
    },
    dango_storage::PrefixBound,
    prost::bytes::Bytes,
    std::sync::Arc,
    tokio::sync::watch,
};

pub type UpgradeHandler<VM> = fn(Box<dyn Storage>, VM, BlockInfo) -> AppResult<()>;

/// Reason why the app requested a graceful halt of the process.
///
/// Sent over a `watch` channel so the binary's main loop can stop the ABCI
/// server, flush the indexer, and exit cleanly with code `0` — in contrast
/// with `panic!`, which leaves async background work unfinished.
#[derive(Clone, Debug)]
pub enum HaltReason {
    /// The chain reached a scheduled upgrade height but the running binary's
    /// cargo version does not match the one declared in the upgrade plan.
    /// The operator must deploy the correct binary before the chain resumes.
    UpgradeIncorrectVersion { current: String, expected: String },
}

/// Handle used by [`App`] to request a graceful shutdown of the host process.
///
/// Cloneable so the `App` (which is itself `Clone`) can carry it across the
/// `split::service` fan-out; all clones drive the same underlying channel.
pub type ShutdownTrigger = Arc<watch::Sender<Option<HaltReason>>>;

/// The ABCI application.
///
/// Must be clonable which is required by `tendermint-abci` library:
/// <https://github.com/informalsystems/tendermint-rs/blob/v0.34.0/abci/src/application.rs#L22-L25>
#[derive(Clone)]
pub struct App<DB, VM, PP = NaiveProposalPreparer, ID = NullIndexer> {
    pub db: DB,
    vm: VM,
    pp: PP,
    pub indexer: ID,

    /// The gas limit when serving ABCI `Query` calls.
    ///
    /// Prevents the situation where an attacker deploys a contract that
    /// contains an extremely expensive query method (such as one containing an
    /// infinite loop), then makes a query request at a node. Without a gas
    /// limit, this can take down the node.
    ///
    /// Note that this is not relevant for queries made as part of a transaction,
    /// which is covered by the transaction's gas limit.
    ///
    /// Related config in CosmWasm:
    /// <https://github.com/CosmWasm/wasmd/blob/v0.51.0/x/wasm/types/types.go#L322-L323>
    query_gas_limit: u64,

    /// The action to take to perform a chain upgrade.
    upgrade_handler: Option<UpgradeHandler<VM>>,

    /// Current cargo version of the app. Used in chain upgrades.
    cargo_version: String,

    /// If set, the app can request a graceful shutdown of the host process
    /// by sending a [`HaltReason`] through this channel (e.g. when
    /// `finalize_block` hits an upgrade height with the wrong binary).
    ///
    /// `None` in tests and in the read-only `App` instance used by the HTTP
    /// server, which never finalize blocks and therefore never halt.
    shutdown_trigger: Option<ShutdownTrigger>,

    /// Number of most-recent blocks CometBFT should retain; older blocks are
    /// pruned. `0` disables pruning (retain all blocks).
    ///
    /// Surfaced to CometBFT as the `retain_height` in the ABCI `Commit`
    /// response (`retain_height = latest_height - retain_recent_blocks`).
    /// Dango doesn't need CometBFT to keep historical blocks, as the embedded
    /// indexer persists every block to disk independently.
    pub(crate) retain_recent_blocks: u64,
}

impl<DB, VM, PP, ID> App<DB, VM, PP, ID> {
    pub fn new<V>(
        db: DB,
        vm: VM,
        pp: PP,
        indexer: ID,
        query_gas_limit: u64,
        upgrade_handler: Option<UpgradeHandler<VM>>,
        cargo_version: V,
    ) -> Self
    where
        V: Into<String>,
    {
        #[cfg(feature = "metrics")]
        {
            crate::metrics::init_metrics();
        }

        Self {
            db,
            vm,
            pp,
            indexer,
            query_gas_limit,
            upgrade_handler,
            cargo_version: cargo_version.into(),
            shutdown_trigger: None,
            retain_recent_blocks: 0,
        }
    }

    /// Attach a shutdown trigger so the app can request a graceful halt of
    /// the host process (see [`HaltReason`]).
    ///
    /// Intended to be called by the binary's `start` command right after
    /// `App::new`; tests and the HTTP-only `App` instance leave this unset.
    #[must_use]
    pub fn with_shutdown_trigger(mut self, trigger: ShutdownTrigger) -> Self {
        self.shutdown_trigger = Some(trigger);
        self
    }

    /// Set the number of most-recent blocks for CometBFT to retain; older
    /// blocks are pruned. `0` (the default) disables pruning.
    ///
    /// Intended to be called by the binary's `start` command right after
    /// `App::new`; tests and the HTTP-only `App` instance leave this unset.
    #[must_use]
    pub fn with_retain_recent_blocks(mut self, retain_recent_blocks: u64) -> Self {
        self.retain_recent_blocks = retain_recent_blocks;
        self
    }
}

#[cfg(feature = "testing")]
impl<DB, VM, PP, ID> App<DB, VM, PP, ID> {
    pub fn set_cargo_version_and_upgrade_handler<V>(
        &mut self,
        cargo_version: V,
        upgrade_handler: Option<UpgradeHandler<VM>>,
    ) where
        V: Into<String>,
    {
        self.cargo_version = cargo_version.into();
        self.upgrade_handler = upgrade_handler;
    }

    pub fn set_shutdown_trigger(&mut self, trigger: ShutdownTrigger) {
        self.shutdown_trigger = Some(trigger);
    }
}

impl<DB, VM, PP, ID> App<DB, VM, PP, ID>
where
    DB: Clone,
    VM: Clone,
    PP: Clone,
{
    pub fn clone_without_indexer(&self) -> App<DB, VM, PP, NullIndexer> {
        App {
            db: self.db.clone(),
            vm: self.vm.clone(),
            pp: self.pp.clone(),
            indexer: NullIndexer,
            query_gas_limit: self.query_gas_limit,
            upgrade_handler: self.upgrade_handler,
            cargo_version: self.cargo_version.clone(),
            shutdown_trigger: self.shutdown_trigger.clone(),
            retain_recent_blocks: self.retain_recent_blocks,
        }
    }
}

impl<DB, VM, PP, ID> App<DB, VM, PP, ID>
where
    DB: Db,
    VM: Vm + Clone + Send + Sync + 'static,
    PP: ProposalPreparer,
    ID: Indexer,
    AppError: From<DB::Error> + From<VM::Error> + From<PP::Error>,
{
    pub fn do_init_chain(
        &self,
        chain_id: String,
        block: BlockInfo,
        genesis_state: GenesisState,
    ) -> AppResult<Hash256> {
        #[cfg(feature = "tracing")]
        {
            tracing::info!(
                chain_id,
                genesis_time = block.timestamp.to_rfc3339_string(),
                "Received InitChain request"
            );
        }

        let mut buffer = Shared::new(Buffer::new(
            self.db.state_storage_with_comment(None, "init_chain")?,
            None,
            "init_chain",
        ));

        // Make sure the genesis block height is zero. This is necessary to
        // ensure that block height always matches the DB version.
        if block.height != 0 {
            return Err(AppError::incorrect_block_height(0, block.height));
        }

        // Create gas tracker for genesis.
        // During genesis, there is no gas limit.
        let gas_tracker = GasTracker::new_limitless();

        // Save the config and genesis block, so that they can be queried when
        // executing genesis messages.
        CHAIN_ID.save(&mut buffer, &chain_id)?;
        LAST_FINALIZED_BLOCK.save(&mut buffer, &block)?;
        CONFIG.save(&mut buffer, &genesis_state.config)?;
        APP_CONFIG.save(&mut buffer, &genesis_state.app_config)?;

        // Schedule cronjobs.
        for (contract, interval) in genesis_state.config.cronjobs {
            schedule_cronjob(&mut buffer, contract, block.timestamp + interval)?;
        }

        // Loop through genesis messages and execute each one.
        //
        // It's expected that genesis messages should all successfully execute.
        // If anyone fails, it's considered fatal and genesis is aborted.
        // The developer should examine the error, fix it, and retry.
        #[cfg_attr(not(feature = "tracing"), allow(clippy::unused_enumerate_index))]
        for (_idx, msg) in genesis_state.msgs.into_iter().enumerate() {
            #[cfg(feature = "tracing")]
            {
                tracing::info!(idx = _idx, "Processing genesis message");
            }

            let output = process_msg(
                self.vm.clone(),
                Box::new(buffer.clone()),
                gas_tracker.clone(),
                block,
                0,
                GENESIS_SENDER,
                msg,
                TraceOption::LOUD,
            );

            if let Err((_event, err)) = output.into_result() {
                #[cfg(feature = "tracing")]
                {
                    tracing::error!(
                        result = _event.to_json_string_pretty().unwrap(),
                        "Error during genesis message processing"
                    );
                }

                return Err(err);
            }
        }

        // Persist the state changes to disk
        let (_, pending) = buffer.disassemble().disassemble();
        let (version, root_hash) = self.db.flush_and_commit(pending)?;

        // Sanity check: DB version should be 0
        debug_assert_eq!(version, 0);

        // Sanity check: the root hash should not be `None`.
        //
        // It's only `None` when the Merkle tree is empty, but we have written
        // some data to it (like chain ID and config) so it shouldn't be empty.
        debug_assert!(root_hash.is_some());

        #[cfg(feature = "tracing")]
        {
            tracing::info!(
                chain_id,
                time = block.timestamp.to_rfc3339_string(),
                app_hash = root_hash.as_ref().unwrap().to_string(),
                gas_used = gas_tracker.used(),
                "Completed genesis"
            );
        }

        Ok(root_hash.unwrap())
    }

    pub fn do_prepare_proposal(&self, txs: Vec<Bytes>, max_tx_bytes: usize) -> Vec<Bytes> {
        #[cfg(feature = "tracing")]
        {
            tracing::info!(
                num_txs = txs.len(),
                max_tx_bytes,
                "Received PrepareProposal request",
            );
        }

        #[cfg(feature = "metrics")]
        let prepare_proposal_duration = std::time::Instant::now();

        #[cfg_attr(not(feature = "tracing"), allow(clippy::unnecessary_lazy_evaluations))]
        let txs = self
            ._do_prepare_proposal(txs.clone(), max_tx_bytes)
            .unwrap_or_else(|_err| {
                #[cfg(feature = "tracing")]
                {
                    tracing::error!(
                        err = _err.to_string(),
                        "Failed to prepare proposal! Falling back to naive preparer."
                    );
                }

                txs
            });

        // Call naive proposal preparer to check the `max_tx_bytes`.
        let bytes = NaiveProposalPreparer
            .prepare_proposal(QuerierWrapper::new(&NaiveQuerier), txs, max_tx_bytes)
            .unwrap();

        #[cfg(feature = "tracing")]
        {
            tracing::info!(bytes = bytes.len(), "Completed PrepareProposal");
        }

        #[cfg(feature = "metrics")]
        {
            metrics::histogram!(crate::metrics::LABEL_DURATION_PREPARE_PROPOSAL)
                .record(prepare_proposal_duration.elapsed().as_secs_f64());
        }

        bytes
    }

    #[inline]
    fn _do_prepare_proposal(&self, txs: Vec<Bytes>, max_tx_bytes: usize) -> AppResult<Vec<Bytes>> {
        let storage = self
            .db
            .state_storage_with_comment(None, "prepare_proposal")?;
        let block = LAST_FINALIZED_BLOCK.load(&storage)?;
        let querier = QuerierProviderImpl::new_boxed(
            self.vm.clone(),
            Box::new(storage),
            GasTracker::new_limitless(),
            block,
        );

        Ok(self
            .pp
            .prepare_proposal(QuerierWrapper::new(&querier), txs, max_tx_bytes)?)
    }

    // Finalize a block by performing the following actions in order:
    //
    // 1. indexer `pre_indexing`
    // 2. execute transactions one by one
    // 3. perform cronjobs
    // 4. remove orphaned nodes
    // 5. flush (but not commit) state changes to DB
    // 5. indexer `index_block`
    pub async fn do_finalize_block(&self, block: Block) -> AppResult<BlockOutcome> {
        #[cfg(feature = "tracing")]
        {
            tracing::info!(
                block_height = block.info.height,
                block_time = block.info.timestamp.to_rfc3339_string(),
                block_hash = block.info.hash.to_string(),
                num_txs = block.txs.len(),
                "Received FinalizeBlock request"
            );
        }

        #[cfg(feature = "metrics")]
        let block_duration = std::time::Instant::now();

        let mut buffer = Shared::new(Buffer::new(
            self.db.state_storage_with_comment(None, "finalize_block")?,
            None,
            "finalize",
        ));

        let last_finalized_block = LAST_FINALIZED_BLOCK.load(&buffer)?;

        // Make sure the new block height is exactly the last finalized height
        // plus one. This ensures that block height always matches the DB version.
        if block.info.height != last_finalized_block.height + 1 {
            #[cfg(feature = "tracing")]
            {
                tracing::error!(
                    expect = last_finalized_block.height + 1,
                    actual = block.info.height,
                    "!!! WRONG BLOCK HEIGHT !!!"
                );
            }

            return Err(AppError::incorrect_block_height(
                last_finalized_block.height + 1,
                block.info.height,
            ));
        }

        // If a planned upgrade exists and the block height is reached, then run
        // the chain upgrade logic.
        if let Some(upgrade) = NEXT_UPGRADE.may_load(&buffer)?
            && block.info.height >= upgrade.height
        {
            // Current node software version doesn't match the upgrade version.
            // Exit early, halt the chain, awaiting the node operator to deploy
            // the right version.
            //
            // See the equivalent code is Cosmos SDK:
            // https://github.com/cosmos/cosmos-sdk/blob/v0.53.4/baseapp/abci.go#L708-L710
            if self.cargo_version != upgrade.cargo_version {
                #[cfg(feature = "tracing")]
                {
                    tracing::warn!(
                        last_finalized_height = last_finalized_block.height,
                        current_version = self.cargo_version,
                        upgrade_version = upgrade.cargo_version,
                        "!!! PRE-PLANNED CHAIN HALT !!!"
                    );
                }

                // Signal the host binary to shut down gracefully (flush the
                // indexer, close HTTP servers, flush telemetry) instead of
                // leaving the main loop to `panic!` in the ABCI layer.
                // Safe to ignore the send result: if there are no receivers,
                // we are running in a test or in an embedded context that
                // does not own the process lifecycle.
                if let Some(trigger) = &self.shutdown_trigger {
                    let _ = trigger.send(Some(HaltReason::UpgradeIncorrectVersion {
                        current: self.cargo_version.clone(),
                        expected: upgrade.cargo_version.clone(),
                    }));
                }

                return Err(AppError::upgrade_incorrect_version(
                    self.cargo_version.clone(),
                    upgrade.cargo_version,
                ));
            }

            // Now the current node software version matches the upgrade version.
            // We can proceed with the upgrade.
            //
            // If an action is specified in the `upgrade_handler`, then execute it.
            // This action MUST succeed. If not, it's considered a critical error.
            // We halt the chain, awaiting a fix.
            if let Some(handler) = self.upgrade_handler {
                #[cfg(feature = "tracing")]
                {
                    tracing::info!(
                        height = block.info.height,
                        cargo_version = upgrade.cargo_version,
                        "Performing chain upgrade"
                    );
                }

                // If executing the action errors, an error is returned in the
                // ABCI response, which leads to a chain halt (as intended).
                (handler)(Box::new(buffer.clone()), self.vm.clone(), block.info).inspect_err(
                    |_err| {
                        #[cfg(feature = "tracing")]
                        {
                            tracing::error!(reason = %_err, "!!! UPGRADE FAILED !!!");
                        }
                    },
                )?;

                #[cfg(feature = "tracing")]
                {
                    tracing::info!(
                        height = block.info.height,
                        cargo_version = upgrade.cargo_version,
                        "Completed chain upgrade"
                    );
                }
            } else {
                #[cfg(feature = "tracing")]
                {
                    tracing::info!(
                        height = block.info.height,
                        cargo_version = upgrade.cargo_version,
                        "No handler specified for the chain upgrade. Skipping..."
                    );
                }
            }

            // Delete the scheduled upgrade, as it's already done.
            NEXT_UPGRADE.remove(&mut buffer);

            // Save the upgrade to the logs.
            PAST_UPGRADES.save(&mut buffer, block.info.height, &upgrade.into())?;
        }

        let mut cron_outcomes = vec![];
        let mut tx_outcomes = vec![];

        self.indexer
            .pre_indexing(block.info.height)
            .await
            .inspect_err(|_err| {
                #[cfg(feature = "tracing")]
                {
                    tracing::error!(err = %_err, "Error in `pre_indexing`");
                }
            })?;

        #[cfg(feature = "metrics")]
        {
            metrics::histogram!(crate::metrics::LABEL_TX_PER_BLOCK).record(block.txs.len() as f64);
        }

        // Process transactions one-by-one.
        #[cfg_attr(not(feature = "tracing"), allow(clippy::unused_enumerate_index))]
        for (_idx, (tx, _)) in block.txs.clone().into_iter().enumerate() {
            #[cfg(feature = "tracing")]
            {
                tracing::info!(idx = _idx, "Processing transaction");
            }

            #[cfg(feature = "metrics")]
            let tx_duration = std::time::Instant::now();

            let tx_outcome = process_tx(
                self.vm.clone(),
                buffer.clone(),
                block.info,
                tx.clone(),
                AuthMode::Finalize,
                TraceOption::LOUD,
            );

            #[cfg(feature = "metrics")]
            {
                metrics::histogram!(crate::metrics::LABEL_DURATION_TX)
                    .record(tx_duration.elapsed().as_secs_f64());

                if tx_outcome.result.is_ok() {
                    metrics::counter!(crate::metrics::LABEL_SUCCESSFUL_TX).increment(1);
                } else {
                    metrics::counter!(crate::metrics::LABEL_FAILED_TX).increment(1);
                }
            };

            tx_outcomes.push(tx_outcome);
        }

        let cfg = CONFIG.load(&buffer)?;

        // Find all cronjobs that should be performed. That is, ones that the
        // scheduled time is earlier or equal to the current block time.
        let jobs = NEXT_CRONJOBS
            .prefix_range(
                &buffer,
                None,
                Some(PrefixBound::Inclusive(block.info.timestamp)),
                Order::Ascending,
            )
            .collect::<StdResult<Vec<_>>>()?;

        // Perform the cronjobs.
        #[cfg_attr(not(feature = "tracing"), allow(clippy::unused_enumerate_index))]
        for (_idx, (time, contract)) in jobs.into_iter().enumerate() {
            #[cfg(feature = "tracing")]
            {
                tracing::info!(
                    idx = _idx,
                    time = time.to_rfc3339_string(),
                    contract = contract.to_string(),
                    "Performing cronjob"
                );
            }

            let cron_buffer = Shared::new(Buffer::new(buffer.clone(), None, "cron"));
            let cron_gas_tracker = GasTracker::new_limitless();
            let next_time = block.info.timestamp + cfg.cronjobs[&contract];

            let cron_event = do_cron_execute(
                self.vm.clone(),
                Box::new(cron_buffer.clone()),
                cron_gas_tracker.clone(),
                block.info,
                contract,
                time,
                next_time,
                TraceOption::LOUD,
            );

            // Commit state changes if the cronjob was successful.
            // Ignore if unsuccessful.
            if cron_event.is_ok() {
                cron_buffer.disassemble().commit();
            }

            cron_outcomes.push(CronOutcome::new(
                cron_gas_tracker.limit(),
                cron_gas_tracker.used(),
                cron_event.into_commitment_status(),
            ));

            // Delete the current cronjob, since it has been completed.
            NEXT_CRONJOBS.remove(&mut buffer, (time, contract));

            // Schedule the next time this cronjob is to be performed.
            schedule_cronjob(&mut buffer, contract, next_time)?;
        }

        // Remove orphaned codes (those that are not used by any contract) that
        // have been orphaned longer than the maximum age.
        if let Some(since) = block
            .info
            .timestamp
            .into_nanos()
            .checked_sub(cfg.max_orphan_age.into_nanos())
        {
            for hash in CODES
                .idx
                .status
                .prefix_keys(
                    &buffer,
                    None,
                    Some(PrefixBound::Inclusive(CodeStatus::Orphaned {
                        since: Duration::from_nanos(since),
                    })),
                    Order::Ascending,
                )
                .map(|res| res.map(|(_status, hash)| hash))
                .collect::<StdResult<Vec<_>>>()?
            {
                #[cfg(feature = "tracing")]
                {
                    tracing::info!(hash = ?hash, "Orphaned code purged");
                }

                CODES.remove(&mut buffer, hash)?;
            }
        }

        // Save the last committed block.
        //
        // Note that we do this _after_ the transactions have been executed.
        // If a contract queries the last committed block during the execution,
        // it gets the previous block, not the current one.
        LAST_FINALIZED_BLOCK.save(&mut buffer, &block.info)?;

        // Flush the state changes to the DB, but keep it in memory, not persist
        // to disk yet. It will be done in the ABCI `Commit` call.
        let (_, batch) = buffer.disassemble().disassemble();
        let (version, app_hash) = self.db.flush_but_not_commit(batch)?;

        // Sanity checks, same as in `do_init_chain`:
        // - Block height matches DB version
        // - Merkle tree isn't empty
        debug_assert_eq!(block.info.height, version);
        debug_assert!(app_hash.is_some());

        #[cfg(feature = "tracing")]
        {
            tracing::info!(
                height = block.info.height,
                time = block.info.timestamp.to_rfc3339_string(),
                app_hash = app_hash.as_ref().unwrap().to_string(),
                "Finalized block"
            );
        }

        let block_outcome = BlockOutcome {
            height: block.info.height,
            app_hash: app_hash.unwrap(),
            cron_outcomes,
            tx_outcomes,
        };

        self.indexer
            .index_block(&block, &block_outcome)
            .await
            .inspect_err(|_err| {
                #[cfg(feature = "tracing")]
                {
                    tracing::error!(err = %_err, "Error in `index_block`");
                }
            })?;

        #[cfg(feature = "metrics")]
        {
            metrics::histogram!(crate::metrics::LABEL_DURATION_BLOCK)
                .record(block_duration.elapsed().as_secs_f64());
        }

        Ok(block_outcome)
    }

    pub async fn do_commit(&self) -> AppResult<u64> {
        #[cfg(feature = "tracing")]
        {
            tracing::info!("Received Commit request");
        }

        #[cfg(feature = "metrics")]
        let commit_duration = std::time::Instant::now();

        let version = self.db.commit()?;

        #[cfg(feature = "tracing")]
        {
            tracing::info!(height = version, "Committed state");
        }

        let storage = self.db.state_storage_with_comment(None, "post_indexing")?;
        let cfg = CONFIG.load(&storage)?;
        let app_cfg = APP_CONFIG.load(&storage)?;

        self.indexer
            .post_indexing(version, cfg, app_cfg)
            .await
            .inspect_err(|_err| {
                #[cfg(feature = "tracing")]
                {
                    tracing::error!(err = %_err, "Error in `post_indexing`");
                }
            })?;

        #[cfg(feature = "metrics")]
        {
            metrics::histogram!(crate::metrics::LABEL_DURATION_COMMIT)
                .record(commit_duration.elapsed().as_secs_f64());
        }

        Ok(version)
    }

    // For `CheckTx`, we only do the first two steps of the transaction
    // processing flow:
    // 1. withhold the fee, making sure the sender has sufficient tokens to cover
    //    the tx fee;
    // 2. `authenticate`, where the sender account authenticates the transaction.
    pub fn do_check_tx(&self, tx: Tx) -> AppResult<CheckTxOutcome> {
        let buffer = Shared::new(Buffer::new(
            self.db.state_storage_with_comment(None, "check_tx")?,
            None,
            "check_tx",
        ));
        let block = LAST_FINALIZED_BLOCK.load(&buffer)?;
        let gas_tracker = GasTracker::new_limited(tx.gas_limit);

        let mut events = CheckTxEvents::new(
            do_withhold_fee(
                self.vm.clone(),
                Box::new(buffer.clone()),
                GasTracker::new_limitless(),
                block,
                &tx,
                AuthMode::Check,
                TraceOption::MUTE,
            )
            .into_commitment_status(),
        );

        if let Err((_, err)) = events.withhold.as_result() {
            return Ok(new_check_tx_outcome(gas_tracker, Err(err.clone()), events));
        }

        events.authenticate = do_authenticate(
            self.vm.clone(),
            Box::new(buffer),
            gas_tracker.clone(),
            block,
            &tx,
            AuthMode::Check,
            TraceOption::MUTE,
        )
        .into_commitment_status();

        let result = if let Err((_, err)) = events.authenticate.as_result() {
            Err(err.clone())
        } else {
            Ok(())
        };

        Ok(new_check_tx_outcome(gas_tracker, result, events))
    }

    // Returns (last_block_height, last_block_app_hash).
    // Note that we are returning the app hash, not the block hash.
    pub fn do_info(&self) -> AppResult<(u64, Hash256)> {
        let Some(version) = self.db.latest_version() else {
            // The DB doesn't have a version yet. This is the case if the chain
            // hasn't started yet (prior to the `InitChain` call). In this case,
            // we return zero height and an all-zero zero hash.
            return Ok((0, Hash256::ZERO));
        };

        let Some(root_hash) = self.db.root_hash(Some(version))? else {
            // Root hash is `None`. Since we know version is not zero at this
            // point, the only way root hash is `None` is that state tree is
            // empty. However this is impossible, since we always keep some data
            // in the state (such as chain ID and config).
            panic!("root hash not found at the latest version ({version})");
        };

        Ok((version, root_hash))
    }

    pub fn do_query_app(&self, req: Query) -> AppResult<QueryResponse> {
        let (res, _height) = self.do_query_app_with_height(req)?;
        Ok(res)
    }

    /// Return the query response, as well as the block height this query was
    /// performed at.
    pub fn do_query_app_with_height(&self, req: Query) -> AppResult<(QueryResponse, u64)> {
        // Use the state storage at the latest version to perform the query.
        let storage = self.db.state_storage_with_comment(None, "query_app")?;
        let block = LAST_FINALIZED_BLOCK.load(&storage)?;

        let res = process_query(
            self.vm.clone(),
            Box::new(storage),
            GasTracker::new_limited(self.query_gas_limit),
            block,
            0,
            req,
        )?;

        Ok((res, block.height))
    }

    pub fn do_simulate(&self, unsigned_tx: UnsignedTx) -> AppResult<TxOutcome> {
        let buffer = Buffer::new(
            self.db.state_storage_with_comment(None, "simulate")?,
            None,
            "simulate",
        );
        let block = LAST_FINALIZED_BLOCK.load(&buffer)?;

        // Create a `Tx` from the unsigned transaction.
        // Use using the node's query gas limit as the transaction gas limit,
        // and empty bytes as credential.
        let tx = Tx {
            sender: unsigned_tx.sender,
            gas_limit: self.query_gas_limit,
            msgs: unsigned_tx.msgs,
            data: unsigned_tx.data,
            credential: Json::null(),
        };

        // Run the transaction with `simulate` as `true`. Track how much gas was
        // consumed, and, if it was successful, what events were emitted.
        Ok(process_tx(
            self.vm.clone(),
            buffer,
            block,
            tx,
            AuthMode::Simulate,
            TraceOption::MUTE, // Mute tracing outputs during simulation.
        ))
    }
}

// These methods use JSON encoding, unlike everywhere else in the app which uses
// Borsh encoding. This is because these are the methods that clients interact
// with, and it's difficult to do Borsh encoding in JS client (JS sucks).
#[cfg(feature = "abci")]
impl<DB, VM, PP, ID> App<DB, VM, PP, ID>
where
    DB: Db,
    VM: Vm + Clone + Send + Sync + 'static,
    PP: ProposalPreparer,
    ID: Indexer,
    AppError: From<DB::Error> + From<VM::Error> + From<PP::Error>,
{
    pub fn do_init_chain_raw(
        &self,
        chain_id: String,
        block: BlockInfo,
        raw_genesis_state: &[u8],
    ) -> AppResult<Hash256> {
        let genesis_state = raw_genesis_state.deserialize_json()?;

        self.do_init_chain(chain_id, block, genesis_state)
    }

    pub async fn do_finalize_block_raw<T>(
        &self,
        block_info: BlockInfo,
        raw_txs: &[T],
    ) -> AppResult<BlockOutcome>
    where
        T: AsRef<[u8]>,
    {
        let txs = raw_txs
            .iter()
            .filter_map(|raw_tx| {
                if let Ok(tx) = raw_tx.deserialize_json() {
                    Some((tx, raw_tx.sha2_256()))
                } else {
                    // The transaction failed to deserialize.
                    //
                    // This can only happen for txs inserted by the block's
                    // proposer during ABCI++ `PrepareProposal`, as regular txs
                    // submitted by users would have been rejected during `CheckTx`
                    // if they fail to deserialize.
                    //
                    // A block proposer inserting an invalid tx is a fatal error.
                    // There's no correct answer on what's the better way to
                    // handle this - halt the chain, or ignore? Here we choose
                    // to ignore.
                    #[cfg(feature = "tracing")]
                    {
                        tracing::error!(
                            raw_tx = BASE64.encode(raw_tx.as_ref()),
                            "Failed to deserialize transaction! Ignoring it..."
                        );
                    }

                    None
                }
            })
            .collect();

        let block = Block {
            info: block_info,
            txs,
        };

        self.do_finalize_block(block).await
    }

    pub fn do_check_tx_raw(&self, raw_tx: &[u8]) -> AppResult<CheckTxOutcome> {
        let tx = raw_tx.deserialize_json()?;

        self.do_check_tx(tx)
    }

    pub fn do_simulate_raw(
        &self,
        raw_unsigned_tx: &[u8],
        height: u64,
        prove: bool,
    ) -> AppResult<Vec<u8>> {
        if height != 0 {
            return Err(AppError::past_height_not_supported());
        }

        if prove {
            return Err(AppError::proof_not_supported());
        }

        let tx = raw_unsigned_tx.deserialize_json()?;
        let res = self.do_simulate(tx)?;

        Ok(res.to_json_vec()?)
    }

    pub fn do_query_app_raw(&self, raw_req: &[u8], height: u64, prove: bool) -> AppResult<Vec<u8>> {
        if height != 0 {
            return Err(AppError::past_height_not_supported());
        };

        if prove {
            return Err(AppError::proof_not_supported());
        }

        let req = raw_req.deserialize_json()?;
        let res = self.do_query_app(req)?;

        Ok(res.to_json_vec()?)
    }
}

fn process_tx<S, VM>(
    vm: VM,
    storage: S,
    block: BlockInfo,
    tx: Tx,
    mode: AuthMode,
    trace_opt: TraceOption,
) -> TxOutcome
where
    S: Storage + Clone + 'static,
    VM: Vm + Clone + Send + Sync + 'static,
    AppError: From<VM::Error>,
{
    // Create the gas tracker, with the limit being the gas limit requested by
    // the transaction.
    let gas_tracker = GasTracker::new_limited(tx.gas_limit);

    // Create two layers of buffers.
    //
    // The 1st layer is for fee handling; the 2nd is for tx authentication and
    // processing of the messages.
    let fee_buffer = Shared::new(Buffer::new(storage.clone(), None, "fee"));
    let msg_buffer = Shared::new(Buffer::new(fee_buffer.clone(), None, "msg"));

    // Record the events emitted during the processing of this transaction.

    // Withhold the transaction fee.
    //
    // The purpose of this step is to ensure the tx's sender has sufficient
    // token balance to cover the maximum possible fee the tx may incur.
    //
    // If this succeeds, record the events emitted.
    //
    // If this fails, we abort the tx and return, discard all state changes.
    let mut events = TxEvents::new(
        do_withhold_fee(
            vm.clone(),
            Box::new(fee_buffer.clone()),
            GasTracker::new_limitless(),
            block,
            &tx,
            mode,
            trace_opt,
        )
        .into_commitment_status(),
    );

    if let Some(err) = events.withhold.maybe_error() {
        let err = err.clone();
        return new_tx_outcome(gas_tracker, events, Err(err));
    }

    // Call the sender account's `authenticate` function.
    //
    // The sender account is supposed to perform authentication here, such as
    // verifying a cryptographic signature, to ensure the tx comes from the
    // sender account's rightful owner.
    //
    // Note that we use `msg_buffer` for this.
    //
    // If succeeds, commit state changes in `msg_buffer` into `fee_buffer`, and record
    // the events emitted.
    //
    // If fails, discard state changes in `msg_buffer` (but keeping those in
    // `fee_buffer`, so the withheld fee is still charged), discard the events,
    // and commit the withheld fee.
    events.authenticate = do_authenticate(
        vm.clone(),
        Box::new(msg_buffer.clone()),
        gas_tracker.clone(),
        block,
        &tx,
        mode,
        trace_opt,
    )
    .into_commitment_status();

    if let Err((_, err)) = events.authenticate.as_result() {
        drop(msg_buffer);
        let err = err.clone();
        return finish_tx(fee_buffer, gas_tracker, events, Err(err));
    }

    msg_buffer.write_access().commit();

    // Loop through the messages and execute one by one.
    //
    // If everything succeeds, commit state changes in `msg_buffer` into `fee_buffer`,
    // and record the events emitted.
    //
    // If anything fails, discard state changes in `msg_buffer` (but keeping those
    // in `fee_buffer`, so the withheld fee is still charged), discard the events,
    // and commit the withheld fee.
    events.msgs_and_backrun = process_msgs(
        vm.clone(),
        msg_buffer.clone(),
        gas_tracker.clone(),
        block,
        &tx,
        trace_opt,
    )
    .into_commitment();

    if let Some(err) = events.msgs_and_backrun.maybe_error() {
        drop(msg_buffer);
        let err = err.clone();
        return finish_tx(fee_buffer, gas_tracker, events, Err(err));
    }

    msg_buffer.disassemble().consume();

    // Everything succeeded. Commit the withheld fee -- along with the
    // authentication and message state changes already merged into `fee_buffer`
    // -- to the underlying storage.
    finish_tx(fee_buffer, gas_tracker, events, Ok(()))
}

#[inline]
fn process_msgs<S, VM>(
    vm: VM,
    buffer: Shared<Buffer<S>>,
    gas_tracker: GasTracker,
    block: BlockInfo,
    tx: &Tx,
    trace_opt: TraceOption,
) -> EventResult<MsgsAndBackrunEvents>
where
    S: Storage + Clone + 'static,
    VM: Vm + Clone + Send + Sync + 'static,
    AppError: From<VM::Error>,
{
    let mut evt = MsgsAndBackrunEvents::base();

    #[cfg_attr(not(feature = "tracing"), allow(clippy::unused_enumerate_index))]
    for (_idx, msg) in tx.msgs.iter().enumerate() {
        #[cfg(feature = "tracing")]
        {
            tracing::info!(idx = _idx, "Processing message");
        }

        catch_and_push_event! {
            process_msg(
                vm.clone(),
                Box::new(buffer.clone()),
                gas_tracker.clone(),
                block,
                0,
                tx.sender,
                msg.clone(),
                trace_opt,
            ),
            evt,
            msgs
        }
    }

    EventResult::Ok(evt)
}

/// Commit the fee buffer -- which holds the withheld gas fee, plus any
/// authentication and message state changes that were merged into it -- to the
/// underlying storage, and build the transaction outcome.
///
/// The withheld fee is committed regardless of whether the transaction
/// ultimately succeeded: a sender that passes fee withholding but fails
/// authentication or message execution still pays for the gas it requested.
fn finish_tx<S>(
    buffer: Shared<Buffer<S>>,
    gas_tracker: GasTracker,
    events: TxEvents,
    result: GenericResult<()>,
) -> TxOutcome
where
    S: Storage + Clone + 'static,
{
    buffer.disassemble().consume();
    new_tx_outcome(gas_tracker, events, result)
}

pub fn process_msg<VM>(
    vm: VM,
    mut storage: Box<dyn Storage>,
    gas_tracker: GasTracker,
    block: BlockInfo,
    msg_depth: usize,
    sender: Addr,
    msg: Message,
    trace_opt: TraceOption,
) -> EventResult<Event>
where
    VM: Vm + Clone + Send + Sync + 'static,
    AppError: From<VM::Error>,
{
    #[cfg(feature = "metrics")]
    {
        metrics::counter!(crate::metrics::LABEL_PROCESSED_MSGS).increment(1);
    }

    match msg {
        Message::Configure(msg) => {
            let res = do_configure(&mut storage, block, sender, msg, trace_opt);
            res.map(Event::Configure)
        },
        Message::Upgrade(msg) => {
            let res = do_upgrade(&mut storage, gas_tracker, block, sender, msg, trace_opt);
            res.map(Event::Upgrade)
        },
        Message::Transfer(msg) => {
            let res = do_transfer(
                vm,
                storage,
                gas_tracker,
                block,
                msg_depth,
                sender,
                msg,
                true,
                trace_opt,
            );
            res.map(Event::Transfer)
        },
        Message::Upload(msg) => {
            let res = do_upload(&mut storage, gas_tracker, block, sender, msg, trace_opt);
            res.map(Event::Upload)
        },
        Message::Instantiate(msg) => {
            let res = do_instantiate(
                vm,
                storage,
                gas_tracker,
                block,
                msg_depth,
                sender,
                msg,
                trace_opt,
            );
            res.map(Event::Instantiate)
        },
        Message::Execute(msg) => {
            let res = do_execute(
                vm,
                storage,
                gas_tracker,
                block,
                msg_depth,
                sender,
                msg,
                trace_opt,
            );
            res.map(Event::Execute)
        },
        Message::Migrate(msg) => {
            let res = do_migrate(
                vm,
                storage,
                gas_tracker,
                block,
                msg_depth,
                sender,
                msg,
                trace_opt,
            );
            res.map(Event::Migrate)
        },
    }
}

pub fn process_query<VM>(
    vm: VM,
    storage: Box<dyn Storage>,
    gas_tracker: GasTracker,
    block: BlockInfo,
    query_depth: usize,
    req: Query,
) -> AppResult<QueryResponse>
where
    VM: Vm + Clone + Send + Sync + 'static,
    AppError: From<VM::Error>,
{
    #[cfg(feature = "metrics")]
    {
        metrics::counter!(crate::metrics::LABEL_PROCESSED_QUERIES).increment(1);
    }

    match req {
        Query::Status(_req) => {
            let res = query_status(&storage, gas_tracker)?;
            Ok(QueryResponse::Status(res))
        },
        Query::Config(_req) => {
            let res = query_config(&storage, gas_tracker)?;
            Ok(QueryResponse::Config(res))
        },
        Query::AppConfig(_req) => {
            let res = query_app_config(&storage, gas_tracker)?;
            Ok(QueryResponse::AppConfig(res))
        },
        Query::NextUpgrade(_req) => {
            let res = query_next_upgrade(&storage, gas_tracker)?;
            Ok(QueryResponse::NextUpgrade(res))
        },
        Query::PastUpgrades(req) => {
            let res = query_past_upgrades(&storage, gas_tracker, req)?;
            Ok(QueryResponse::PastUpgrades(res))
        },
        Query::Balance(req) => {
            let res = query_balance(vm, storage, gas_tracker, block, query_depth, req)?;
            Ok(QueryResponse::Balance(res))
        },
        Query::Balances(req) => {
            let res = query_balances(vm, storage, gas_tracker, block, query_depth, req)?;
            Ok(QueryResponse::Balances(res))
        },
        Query::Supply(req) => {
            let res = query_supply(vm, storage, gas_tracker, block, query_depth, req)?;
            Ok(QueryResponse::Supply(res))
        },
        Query::Supplies(req) => {
            let res = query_supplies(vm, storage, gas_tracker, block, query_depth, req)?;
            Ok(QueryResponse::Supplies(res))
        },
        Query::Code(req) => {
            let res = query_code(&storage, gas_tracker, req)?;
            Ok(QueryResponse::Code(res))
        },
        Query::Codes(req) => {
            let res = query_codes(&storage, gas_tracker, req)?;
            Ok(QueryResponse::Codes(res))
        },
        Query::Contract(req) => {
            let res = query_contract(&storage, gas_tracker, req)?;
            Ok(QueryResponse::Contract(res))
        },
        Query::Contracts(req) => {
            let res = query_contracts(&storage, gas_tracker, req)?;
            Ok(QueryResponse::Contracts(res))
        },
        Query::WasmRaw(req) => {
            let res = query_wasm_raw(storage, gas_tracker, req)?;
            Ok(QueryResponse::WasmRaw(res))
        },
        Query::WasmScan(req) => {
            let res = query_wasm_scan(storage, gas_tracker, req)?;
            Ok(QueryResponse::WasmScan(res))
        },
        Query::WasmSmart(req) => {
            let res = query_wasm_smart(vm, storage, gas_tracker, block, query_depth, req)?;
            Ok(QueryResponse::WasmSmart(res))
        },
        Query::Multi(reqs) => {
            let res = reqs
                .into_iter()
                .map(|req| {
                    if matches!(req, Query::Multi(_)) {
                        return Err(AppError::nested_multi_query()).into_generic_result();
                    }

                    process_query(
                        vm.clone(),
                        storage.clone(),
                        gas_tracker.clone(),
                        block,
                        query_depth,
                        req,
                    )
                    .into_generic_result()
                })
                .collect::<Vec<_>>();
            Ok(QueryResponse::Multi(res))
        },
    }
}

pub(crate) fn has_permission(permission: &Permission, owner: Addr, sender: Addr) -> bool {
    // The genesis sender can always store code and instantiate contracts.
    if sender == GENESIS_SENDER {
        return true;
    }

    // The owner can always do anything it wants.
    if sender == owner {
        return true;
    }

    match permission {
        Permission::Nobody => false,
        Permission::Everybody => true,
        Permission::Somebodies(accounts) => accounts.contains(&sender),
    }
}

pub(crate) fn schedule_cronjob(
    storage: &mut dyn Storage,
    contract: Addr,
    next_time: Timestamp,
) -> StdResult<()> {
    #[cfg(feature = "tracing")]
    {
        tracing::info!(
            time = next_time.to_rfc3339_string(),
            contract = contract.to_string(),
            "Scheduled cronjob"
        );
    }

    NEXT_CRONJOBS.insert(storage, (next_time, contract))
}

fn new_check_tx_outcome(
    gas_tracker: GasTracker,
    result: GenericResult<()>,
    events: CheckTxEvents,
) -> CheckTxOutcome {
    CheckTxOutcome {
        gas_limit: gas_tracker.limit().unwrap(),
        gas_used: gas_tracker.used(),
        result,
        events,
    }
}

fn new_tx_outcome(
    gas_tracker: GasTracker,
    events: TxEvents,
    result: GenericResult<()>,
) -> TxOutcome {
    TxOutcome {
        gas_limit: gas_tracker.limit().unwrap(),
        gas_used: gas_tracker.used(),
        events,
        result,
    }
}
