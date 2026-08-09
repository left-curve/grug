use {
    crate::{MockValidatorSets, constants::MOCK_HYPERLANE_LOCAL_DOMAIN, suite::TestSuite},
    anyhow::anyhow,
    dango_app::{AppError, Db, Indexer, NaiveProposalPreparer, NullIndexer, ProposalPreparer, Vm},
    dango_db_memory::MemDb,
    dango_genesis::Contracts,
    dango_hyperlane_types::{Addr32, mailbox},
    dango_math::Uint128,
    dango_primitives::{Addr, Addressable, Coins, Hash256, Signer, TxOutcome},
    dango_types::{gateway::Domain, warp::TokenMessage},
    dango_vm_rust::RustVm,
    std::ops::{Deref, DerefMut},
};

/// The Hyperlane wrapper around the default `TestSuite` returned by
/// `setup_test` (which uses the Pyth proposal preparer, unlike this
/// struct's `Naive` default).
pub type DefaultHyperlaneTestSuite = HyperlaneTestSuite<
    MemDb,
    RustVm,
    dango_proposal_preparer::ProposalPreparer<dango_pyth_client::PythClientCache>,
    NullIndexer,
>;

pub struct HyperlaneTestSuite<DB = MemDb, VM = RustVm, PP = NaiveProposalPreparer, ID = NullIndexer>
where
    DB: Db,
    VM: Vm + Clone + 'static,
    PP: ProposalPreparer,
    ID: Indexer,
    AppError: From<DB::Error> + From<VM::Error> + From<PP::Error>,
{
    suite: TestSuite<DB, VM, PP, ID>,
    validator_sets: MockValidatorSets,
    mailbox: Addr,
    warp: Addr,
}

impl<DB, VM, PP, ID> Deref for HyperlaneTestSuite<DB, VM, PP, ID>
where
    DB: Db,
    VM: Vm + Clone + 'static,
    PP: ProposalPreparer,
    ID: Indexer,
    AppError: From<DB::Error> + From<VM::Error> + From<PP::Error>,
{
    type Target = TestSuite<DB, VM, PP, ID>;

    fn deref(&self) -> &Self::Target {
        &self.suite
    }
}

impl<DB, VM, PP, ID> DerefMut for HyperlaneTestSuite<DB, VM, PP, ID>
where
    DB: Db,
    VM: Vm + Clone + 'static,
    PP: ProposalPreparer,
    ID: Indexer,
    AppError: From<DB::Error> + From<VM::Error> + From<PP::Error>,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.suite
    }
}

impl<DB, VM, PP, ID> HyperlaneTestSuite<DB, VM, PP, ID>
where
    DB: Db,
    VM: Vm + Clone + Send + Sync + 'static,
    PP: ProposalPreparer,
    ID: Indexer,
    AppError: From<DB::Error> + From<VM::Error> + From<PP::Error>,
{
    pub fn new(
        suite: TestSuite<DB, VM, PP, ID>,
        validator_sets: MockValidatorSets,
        contracts: &Contracts,
    ) -> Self {
        Self {
            suite,
            validator_sets,
            mailbox: contracts.hyperlane.mailbox,
            warp: contracts.warp,
        }
    }

    pub async fn receive_warp_transfer<R, A>(
        &mut self,
        relayer: &mut (dyn Signer + Send + Sync),
        origin_domain: Domain,
        origin_warp: Addr32,
        recipient: &R,
        amount: A,
    ) -> anyhow::Result<Hash256>
    where
        R: Addressable,
        A: Into<Uint128>,
    {
        self.receive_warp_transfer_with_outcome(
            relayer,
            origin_domain,
            origin_warp,
            recipient,
            amount,
        )
        .await
        .map(|(message_id, _)| message_id)
    }

    /// Same as `receive_warp_transfer`, but also returns the outcome of the
    /// mailbox `process` transaction, so callers can inspect the events it
    /// emitted.
    pub async fn receive_warp_transfer_with_outcome<R, A>(
        &mut self,
        relayer: &mut (dyn Signer + Send + Sync),
        origin_domain: Domain,
        origin_warp: Addr32,
        recipient: &R,
        amount: A,
    ) -> anyhow::Result<(Hash256, TxOutcome)>
    where
        R: Addressable,
        A: Into<Uint128>,
    {
        // Mock validator set signs the message.
        let (message_id, raw_message, raw_metadata) = self
            .validator_sets
            .get(origin_domain)
            .ok_or_else(|| {
                anyhow!(
                    "[HyperlaneTestSuite]: no mock validator set found for domain `{origin_domain}`"
                )
            })?
            .sign(
                origin_warp,
                MOCK_HYPERLANE_LOCAL_DOMAIN,
                self.warp,
                TokenMessage {
                    recipient: recipient.address().into(),
                    amount: amount.into(),
                    metadata: Default::default(),
                }
                .encode(),
            );

        // Deliver the message to Dango mailbox.
        let outcome = self
            .suite
            .execute(
                relayer,
                self.mailbox,
                &mailbox::ExecuteMsg::Process {
                    raw_message,
                    raw_metadata,
                },
                Coins::new(),
            )
            .await;

        if let Err(err) = &outcome.result {
            return Err(anyhow!(err.clone()));
        }

        // Return the message ID along with the transaction outcome.
        Ok((message_id, outcome))
    }
}
