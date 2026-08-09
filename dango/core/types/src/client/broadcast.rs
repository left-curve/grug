use {
    crate::{
        Addr, AdminOption, Binary, BroadcastTxOutcome, Coins, Config, GasOption, GenericResult,
        Hash256, HashExt, Message, NonEmpty, QueryClient, Signer, StdError, Tx, TxOutcome,
    },
    async_trait::async_trait,
    dango_backtrace::BacktracedError,
    serde::Serialize,
};

#[async_trait]
pub trait BroadcastClient {
    type Error;

    async fn broadcast_tx(&self, tx: Tx) -> Result<BroadcastTxOutcome, Self::Error>;
}

#[async_trait]
pub trait BroadcastClientExt: BroadcastClient + QueryClient
where
    <Self as BroadcastClient>::Error:
        From<GasEstimateError> + From<StdError> + From<<Self as QueryClient>::Error>,
{
    async fn broadcast_tx_with_confirmation<F>(
        &self,
        tx: Tx,
        confirm_fn: F,
    ) -> Result<Option<BroadcastTxOutcome>, <Self as BroadcastClient>::Error>
    where
        F: Fn(&Tx) -> Result<bool, <Self as BroadcastClient>::Error> + Send + Sync,
    {
        if confirm_fn(&tx)? {
            self.broadcast_tx(tx).await.map(Some)
        } else {
            Ok(None)
        }
    }

    /// Create, sign, and broadcast a transaction with a single message, without
    /// terminal prompt for confirmation.
    ///
    /// If you need the prompt confirmation, use `send_message_with_confirmation`.
    async fn send_message<S>(
        &self,
        signer: &mut S,
        msg: Message,
        gas_opt: GasOption,
        chain_id: &str,
    ) -> Result<BroadcastTxOutcome, <Self as BroadcastClient>::Error>
    where
        S: Signer + Send + Sync,
    {
        self.send_messages(
            signer,
            NonEmpty::new_unchecked(vec![msg]),
            gas_opt,
            chain_id,
        )
        .await
    }

    /// Create, sign, and broadcast a transaction with a single message, with
    /// terminal prompt for confirmation.
    ///
    /// Returns `None` if the prompt is denied.
    async fn send_message_with_confirmation<S, F>(
        &self,
        signer: &mut S,
        msg: Message,
        gas_opt: GasOption,
        chain_id: &str,
        confirm_fn: F,
    ) -> Result<Option<BroadcastTxOutcome>, <Self as BroadcastClient>::Error>
    where
        S: Signer + Send + Sync,
        F: Fn(&Tx) -> Result<bool, <Self as BroadcastClient>::Error> + Send + Sync,
    {
        self.send_messages_with_confirmation(
            signer,
            NonEmpty::new_unchecked(vec![msg]),
            gas_opt,
            chain_id,
            confirm_fn,
        )
        .await
    }

    /// Create, sign, and broadcast a transaction with the given messages,
    /// without terminal prompt for confirmation.
    ///
    /// If you need the prompt confirmation, use `send_messages_with_confirmation`.
    async fn send_messages<S>(
        &self,
        signer: &mut S,
        msgs: NonEmpty<Vec<Message>>,
        gas_opt: GasOption,
        chain_id: &str,
    ) -> Result<BroadcastTxOutcome, <Self as BroadcastClient>::Error>
    where
        S: Signer + Send + Sync,
    {
        self.send_messages_with_confirmation(signer, msgs, gas_opt, chain_id, no_confirmation)
            .await
            .map(Option::unwrap)
    }

    /// Create, sign, and broadcast a transaction with the given messages, with
    /// terminal prompt for confirmation.
    ///
    /// Returns `None` if the prompt is denied.
    async fn send_messages_with_confirmation<S, F>(
        &self,
        signer: &mut S,
        msgs: NonEmpty<Vec<Message>>,
        gas_opt: GasOption,
        chain_id: &str,
        confirm_fn: F,
    ) -> Result<Option<BroadcastTxOutcome>, <Self as BroadcastClient>::Error>
    where
        S: Signer + Send + Sync,
        F: Fn(&Tx) -> Result<bool, <Self as BroadcastClient>::Error> + Send + Sync,
    {
        // If gas limit is not provided, simulate
        let gas_limit = match gas_opt {
            GasOption::Simulate {
                flat_increase,
                scale,
            } => {
                let unsigned_tx = signer.unsigned_transaction(msgs.clone(), chain_id)?;
                match self.simulate(unsigned_tx).await? {
                    TxOutcome {
                        result: GenericResult::Ok(_),
                        gas_used,
                        ..
                    } => (gas_used as f64 * scale).ceil() as u64 + flat_increase,
                    TxOutcome {
                        result: GenericResult::Err(err),
                        ..
                    } => return Err(GasEstimateError(err).into()),
                }
            },
            GasOption::Predefined { gas_limit } => gas_limit,
        };

        let tx = signer.sign_transaction(msgs, chain_id, gas_limit)?;

        self.broadcast_tx_with_confirmation(tx, confirm_fn).await
    }

    /// Send a transaction with a single [`Message::Configure`](dango_primitives::Message::Configure).
    async fn configure<S, T>(
        &self,
        signer: &mut S,
        new_cfg: Option<Config>,
        new_app_cfg: Option<T>,
        gas_opt: GasOption,
        chain_id: &str,
    ) -> Result<BroadcastTxOutcome, <Self as BroadcastClient>::Error>
    where
        S: Signer + Send + Sync,
        T: Serialize + Send,
    {
        let msg = Message::configure(new_cfg, new_app_cfg)?;
        self.send_message(signer, msg, gas_opt, chain_id).await
    }

    /// Send a transaction with a single [`Message::Upgrade`](dango_primitives::Message::Upgrade).
    async fn upgrade<S, T, U, V>(
        &self,
        signer: &mut S,
        cargo_version: T,
        height: u64,
        git_tag: Option<U>,
        url: Option<V>,
        gas_opt: GasOption,
        chain_id: &str,
    ) -> Result<BroadcastTxOutcome, <Self as BroadcastClient>::Error>
    where
        S: Signer + Send + Sync,
        T: Into<String> + Send,
        U: Into<String> + Send,
        V: Into<String> + Send,
    {
        let msg = Message::upgrade(height, cargo_version, git_tag, url);
        self.send_message(signer, msg, gas_opt, chain_id).await
    }

    /// Send a transaction with a single [`Message::Transfer`](dango_primitives::Message::Transfer).
    async fn transfer<S, C>(
        &self,
        signer: &mut S,
        to: Addr,
        coins: C,
        gas_opt: GasOption,
        chain_id: &str,
    ) -> Result<BroadcastTxOutcome, <Self as BroadcastClient>::Error>
    where
        S: Signer + Send + Sync,
        C: TryInto<Coins> + Send,
        StdError: From<C::Error>,
    {
        let msg = Message::transfer(to, coins)?;
        self.send_message(signer, msg, gas_opt, chain_id).await
    }

    /// Send a transaction with a single [`Message::Upload`](dango_primitives::Message::Upload).
    async fn upload<S, B>(
        &self,
        signer: &mut S,
        code: B,
        gas_opt: GasOption,
        chain_id: &str,
    ) -> Result<BroadcastTxOutcome, <Self as BroadcastClient>::Error>
    where
        S: Signer + Send + Sync,
        B: Into<Binary> + Send,
    {
        let msg = Message::upload(code);
        self.send_message(signer, msg, gas_opt, chain_id).await
    }

    /// Send a transaction with a single [`Message::Instantiate`](dango_primitives::Message::Instantiate).
    ///
    /// Return the deployed contract's address.
    async fn instantiate<S, M, SA, C>(
        &self,
        signer: &mut S,
        code_hash: Hash256,
        msg: &M,
        salt: SA,
        label: Option<&str>,
        funds: C,
        gas_opt: GasOption,
        admin_opt: AdminOption,
        chain_id: &str,
    ) -> Result<(Addr, BroadcastTxOutcome), <Self as BroadcastClient>::Error>
    where
        S: Signer + Send + Sync,
        M: Serialize + Send + Sync,
        SA: Into<Binary> + Send,
        C: TryInto<Coins> + Send,
        StdError: From<C::Error>,
    {
        let salt = salt.into();
        let address = Addr::derive(signer.address(), code_hash, &salt);
        let admin = admin_opt.decide(address);

        let msg = Message::instantiate(code_hash, msg, salt, label, admin, funds)?;
        let res = self.send_message(signer, msg, gas_opt, chain_id).await?;

        Ok((address, res))
    }

    /// Send a transaction that uploads a Wasm code, then instantiate a contract
    /// with the code in one go.
    ///
    /// Return the code hash, and the deployed contract's address.
    async fn upload_and_instantiate<S, M, B, SA, C>(
        &self,
        signer: &mut S,
        code: B,
        msg: &M,
        salt: SA,
        label: Option<&str>,
        funds: C,
        gas_opt: GasOption,
        admin_opt: AdminOption,
        chain_id: &str,
    ) -> Result<(Hash256, Addr, BroadcastTxOutcome), <Self as BroadcastClient>::Error>
    where
        S: Signer + Send + Sync,
        M: Serialize + Send + Sync,
        B: Into<Binary> + Send,
        SA: Into<Binary> + Send,
        C: TryInto<Coins> + Send,
        StdError: From<C::Error>,
    {
        let code = code.into();
        let code_hash = code.sha2_256();
        let salt = salt.into();
        let address = Addr::derive(signer.address(), code_hash, &salt);
        let admin = admin_opt.decide(address);

        let msgs = NonEmpty::new_unchecked(vec![
            Message::upload(code),
            Message::instantiate(code_hash, msg, salt, label, admin, funds)?,
        ]);
        let res = self.send_messages(signer, msgs, gas_opt, chain_id).await?;

        Ok((code_hash, address, res))
    }

    /// Send a transaction with a single [`Message::Execute`](dango_primitives::Message::Execute).
    async fn execute<S, M, C>(
        &self,
        signer: &mut S,
        contract: Addr,
        msg: &M,
        funds: C,
        gas_opt: GasOption,
        chain_id: &str,
    ) -> Result<BroadcastTxOutcome, <Self as BroadcastClient>::Error>
    where
        S: Signer + Send + Sync,
        M: Serialize + Send + Sync,
        C: TryInto<Coins> + Send,
        StdError: From<C::Error>,
    {
        let msg = Message::execute(contract, msg, funds)?;
        self.send_message(signer, msg, gas_opt, chain_id).await
    }

    /// Send a transaction with a single [`Message::Migrate`](dango_primitives::Message::Migrate).
    async fn migrate<S, M>(
        &self,
        signer: &mut S,
        contract: Addr,
        new_code_hash: Hash256,
        msg: &M,
        gas_opt: GasOption,
        chain_id: &str,
    ) -> Result<BroadcastTxOutcome, <Self as BroadcastClient>::Error>
    where
        S: Signer + Send + Sync,
        M: Serialize + Send + Sync,
    {
        let msg = Message::migrate(contract, new_code_hash, msg)?;
        self.send_message(signer, msg, gas_opt, chain_id).await
    }
}

impl<C> BroadcastClientExt for C
where
    C: BroadcastClient + QueryClient + Send + Sync,
    <C as BroadcastClient>::Error:
        From<GasEstimateError> + From<StdError> + From<<C as QueryClient>::Error>,
{
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("failed to estimate gas consumption: {0}")]
pub struct GasEstimateError(BacktracedError<String>);

/// Skip the CLI prompt confirmation, always consider it as if the user accepted.
fn no_confirmation<E>(_tx: &Tx) -> Result<bool, E> {
    Ok(true)
}
