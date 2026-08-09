use {
    crate::suite::TestSuite,
    dango_app::{AppError, Db, Indexer, ProposalPreparer, Vm},
    dango_hyperlane_types::Addr32,
    dango_primitives::{
        Addr, CheckedContractEvent, Coins, JsonDeExt, ResultExt, SearchEvent, Signer, StdError,
        TxOutcome,
    },
    dango_types::gateway::{self, Remote, WithdrawalRequested, WithdrawalResponse},
};

impl<DB, VM, PP, ID> TestSuite<DB, VM, PP, ID>
where
    DB: Db,
    VM: Vm + Clone + Send + Sync + 'static,
    PP: ProposalPreparer,
    ID: Indexer,
    AppError: From<DB::Error> + From<VM::Error> + From<PP::Error>,
{
    /// Execute a `TransferRemote` on the gateway, asserting it succeeds, and
    /// return the ID of the withdrawal request it created.
    pub async fn request_transfer_remote<C>(
        &mut self,
        sender: &mut (dyn Signer + Send + Sync),
        gateway: Addr,
        remote: Remote,
        recipient: Addr32,
        funds: C,
    ) -> u64
    where
        C: TryInto<Coins>,
        StdError: From<C::Error>,
    {
        self.execute(
            sender,
            gateway,
            &gateway::ExecuteMsg::TransferRemote { remote, recipient },
            funds,
        )
        .await
        .should_succeed()
        .events
        .search_event::<CheckedContractEvent>()
        .with_predicate(move |e| e.contract == gateway && e.ty == "withdrawal_requested")
        .take()
        .one()
        .event
        .data
        .deserialize_json::<WithdrawalRequested>()
        .unwrap()
        .id
    }

    /// Respond to a withdrawal request on the gateway; return the outcome of
    /// the response transaction.
    pub async fn respond_to_withdrawal(
        &mut self,
        guardian: &mut (dyn Signer + Send + Sync),
        gateway: Addr,
        id: u64,
        response: WithdrawalResponse,
    ) -> TxOutcome {
        self.execute(
            guardian,
            gateway,
            &gateway::ExecuteMsg::RespondToWithdrawal { id, response },
            Coins::new(),
        )
        .await
    }

    /// Execute a `TransferRemote` and, if the request goes through,
    /// immediately approve the resulting withdrawal request as `guardian`;
    /// return the outcome of whichever transaction settled the withdrawal.
    ///
    /// This restores the pre-guardian atomic semantics — a failed withdrawal
    /// leaves no trace — letting tests assert errors and balances the same
    /// way they did when `TransferRemote` was a single step:
    ///
    /// - a request rejected by a fail-fast check (missing route, fee,
    ///   reserve, rate limit) returns the failed request outcome directly;
    /// - an approval that fails validation refunds the escrow on-chain (the
    ///   approval tx itself succeeds, emitting `withdrawal_approval_failed`);
    /// - a hard approval failure (e.g. a bridge-message error) returns the
    ///   failed approval outcome, after rejecting the request so the
    ///   escrowed funds return to the sender.
    pub async fn transfer_remote<C>(
        &mut self,
        sender: &mut (dyn Signer + Send + Sync),
        guardian: &mut (dyn Signer + Send + Sync),
        gateway: Addr,
        remote: Remote,
        recipient: Addr32,
        funds: C,
    ) -> TxOutcome
    where
        C: TryInto<Coins>,
        StdError: From<C::Error>,
    {
        let outcome = self
            .execute(
                sender,
                gateway,
                &gateway::ExecuteMsg::TransferRemote { remote, recipient },
                funds,
            )
            .await;

        if outcome.result.is_err() {
            return outcome;
        }

        let id = outcome
            .events
            .clone()
            .search_event::<CheckedContractEvent>()
            .with_predicate(move |e| e.contract == gateway && e.ty == "withdrawal_requested")
            .take()
            .one()
            .event
            .data
            .deserialize_json::<WithdrawalRequested>()
            .unwrap()
            .id;

        let outcome = self
            .respond_to_withdrawal(guardian, gateway, id, WithdrawalResponse::Approve)
            .await;

        if outcome.result.is_err() {
            self.respond_to_withdrawal(guardian, gateway, id, WithdrawalResponse::Reject)
                .await
                .should_succeed();
        }

        outcome
    }
}
