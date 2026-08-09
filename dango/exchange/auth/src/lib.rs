use {
    alloy::{
        dyn_abi::{Eip712Domain, Resolver, TypedData},
        primitives::{U160, U256, address, uint},
    },
    anyhow::{anyhow, bail, ensure},
    dango_primitives::{
        Addr, Api, AuthCtx, AuthMode, ByteArray, Coins, GENESIS_BLOCK_HEIGHT, Inner, JsonDeExt,
        JsonSerExt, MutableCtx, QuerierExt, QuerierWrapper, SignData, StdError, StdResult, Storage,
        Tx, json,
    },
    dango_storage::StorageQuerier,
    dango_types::{
        DangoQuerier,
        account_factory::{RegisterUserData, User},
        auth::{
            AccountStatus, ClientData, Credential, Key, Metadata, Nonce, SessionInfo, SignDoc,
            Signature, StandardCredential,
        },
    },
    data_encoding::BASE64URL_NOPAD,
    serde_json::Value as JsonValue,
    sha2::Sha256,
    std::collections::BTreeSet,
};

/// The expected storage layout of the account factory contract.
pub mod account_factory {
    use {
        dango_storage::Map,
        dango_types::account_factory::{User, UserIndex},
    };

    pub const USERS: Map<UserIndex, User> = Map::new("user");
}

/// The expected storage layout of the account contract.
pub mod account {
    use {
        dango_storage::{Item, Map},
        dango_types::auth::{AccountStatus, Nonce, SessionKey},
        std::collections::BTreeSet,
    };

    /// The account's status. Only accounts in the `Active` state can send
    /// transactions.
    ///
    /// Upon creation, an account is initialized to the `Inactive` state. It
    /// must receive transfer equal to or greater than the minimum deposit
    /// (specified in the app-config) to become `Active`.
    ///
    /// If this storage slot is empty, it's default to the `Inactive` state.
    pub const STATUS: Item<AccountStatus> = Item::new("status");

    /// The most recent nonces used by *standard* (master-key) credentials.
    ///
    /// Both account types (single, multi) store their standard nonces in this
    /// same storage slot. Session credentials instead use
    /// [`SESSION_SEEN_NONCES`], keyed by the session public key, so that
    /// independent session keys (e.g. one per trading bot) don't collide in a
    /// shared window.
    pub const SEEN_NONCES: Item<BTreeSet<Nonce>> = Item::new("seen_nonces");

    /// The most recent nonces used by each *session* key, keyed by the session
    /// public key.
    ///
    /// Each session key gets its own sliding window, with the same semantics as
    /// `SEEN_NONCES`. Entries are never pruned; each window is bounded at
    /// `MAX_SEEN_NONCES` nonces.
    pub const SESSION_SEEN_NONCES: Map<SessionKey, BTreeSet<Nonce>> =
        Map::new("session_seen_nonces");
}

/// The [EIP-155](https://github.com/ethereum/EIPs/blob/master/EIPS/eip-155.md)
/// chain ID of Ethereum mainnet.
///
/// The [EIP-712](https://github.com/ethereum/EIPs/blob/master/EIPS/eip-712.md#definition-of-domainseparator)
/// standard requires the `chainId` field in the domain. Some wallets enforce
/// this requirement.
///
/// Since Dango isn't an EVM chain and hence doesn't have an EIP-155 chain ID,
/// we use that of Ethereum mainnet for compatibility.
pub const EIP155_CHAIN_ID: U256 = uint!(0x1_U256);

/// Max number of tracked nonces.
pub const MAX_SEEN_NONCES: usize = 20;

/// The maximum difference betwen the nonce of an incoming transaction, and the
/// biggest seen nonce so far.
///
/// This is to prevent a specific DoS attack. A rogue member of a multisig can
/// submit a batch of transactions, such that the `SEEN_NONCES` set is fully
/// filled with the following nonces:
///
/// ```plain
/// (u32::MAX - MAX_SEEN_NONCES + 1)..=u32::MAX
/// ```
///
/// This prevents the multisig from being able to submit any more transactions,
/// because a new tx must come with a nonce bigger than `u32::MAX`, which is
/// impossible.
///
/// We prevent this attack by requiring the nonce of a new tx must not be too
/// much bigger than the biggest nonce seen so far.
pub const MAX_NONCE_INCREASE: Nonce = 100;

// -------------------------------- nonce logic --------------------------------

/// Check whether `nonce` is acceptable against the sliding window `set`, and if
/// so, record it (evicting the oldest entry when the window is full).
///
/// `may_load_floor` is invoked lazily — only when `set` is empty — and returns
/// the account's standard-nonce high-water mark, if any. When present, the
/// first nonce in a fresh window must exceed it; this rejects replays of
/// pre-split session transactions (whose nonces previously lived in the shared
/// `SEEN_NONCES`) while letting clients that pick `max + 1` continue seamlessly.
/// When absent (the account has never transacted), the original "first nonce
/// close to zero" rule applies.
fn check_and_record_nonce<F>(
    set: &mut BTreeSet<Nonce>,
    nonce: Nonce,
    may_load_floor: F,
) -> anyhow::Result<()>
where
    F: FnOnce() -> StdResult<Option<Nonce>>,
{
    if let Some(&oldest) = set.first() {
        // The nonce must not have been seen, and must be newer than the oldest
        // nonce still in the window.
        ensure!(!set.contains(&nonce), "nonce is already seen: {}", nonce);

        ensure!(nonce > oldest, "nonce is too old: {} < {}", nonce, oldest);

        // The nonce must not be too much bigger than the biggest nonce seen so
        // far. See the documentation for `MAX_NONCE_INCREASE` for the rationale.
        // Popping the oldest (smallest) entry below doesn't change the largest,
        // so it's safe to read it here.
        let newest = *set.last().unwrap();
        ensure!(
            nonce <= newest.saturating_add(MAX_NONCE_INCREASE),
            "nonce is too far ahead: {} > {} + MAX_NONCE_INCREASE ({})",
            nonce,
            newest,
            MAX_NONCE_INCREASE
        );

        // Remove the oldest nonce if max capacity is reached.
        if set.len() == MAX_SEEN_NONCES {
            set.pop_first();
        }
    } else {
        // The window is empty. Apply the floor if there is one; otherwise
        // require the first nonce to be close to zero.
        match may_load_floor()? {
            Some(hwm) => ensure!(
                nonce > hwm,
                "first session nonce is too old: {} <= account high-water mark {}",
                nonce,
                hwm
            ),
            None => ensure!(
                nonce < MAX_NONCE_INCREASE,
                "first nonce is too big: {} >= MAX_NONCE_INCREASE ({})",
                nonce,
                MAX_NONCE_INCREASE
            ),
        }
    }

    set.insert(nonce);

    Ok(())
}

/// Query the account's status.
pub fn query_status(storage: &dyn Storage) -> StdResult<AccountStatus> {
    account::STATUS
        .may_load(storage)
        .map(|opt| opt.unwrap_or_default()) // default to to `Inactive` state
}

/// Query the set of most recent nonces tracked (standard credentials).
pub fn query_seen_nonces(storage: &dyn Storage) -> StdResult<BTreeSet<Nonce>> {
    account::SEEN_NONCES
        .may_load(storage)
        .map(|opt| opt.unwrap_or_default()) // default to an empty B-tree set
}

/// Query the set of most recent nonces tracked for a given session key.
pub fn query_session_seen_nonces(
    storage: &dyn Storage,
    session_key: ByteArray<33>,
) -> StdResult<BTreeSet<Nonce>> {
    account::SESSION_SEEN_NONCES
        .may_load(storage, session_key)
        .map(|opt| opt.unwrap_or_default()) // default to an empty B-tree set
}

pub fn create_account(ctx: MutableCtx, activate: bool) -> anyhow::Result<()> {
    let app_cfg = ctx.querier.query_dango_config()?;

    // Only the account factory can create new accounts.
    ensure!(
        ctx.sender == app_cfg.addresses.account_factory,
        "you don't have the right, O you don't have the right"
    );

    // Upon creation, the account's status is set to `Inactive`.
    // We don't need to save it in storage, because if storage is empty, it's
    // default to `Inactive`. This is an intentional optimization to minimize
    // disk writes.
    //
    // Exceptions to this are:
    // 1. account factory has specified in the instantiate message that the
    //    account is to be activated upon instantiation;
    // 2. during genesis (genesis accounts are always activated);
    // 3. the account received sufficient funds during instantiation.
    // In these cases, activate the account now.
    if activate
        || ctx.block.height == GENESIS_BLOCK_HEIGHT
        || is_sufficient_deposit(&ctx.funds, &app_cfg.minimum_deposit)
    {
        account::STATUS.save(ctx.storage, &AccountStatus::Active)?;
        // TODO: emit an event?
    }

    Ok(())
}

pub fn receive_transfer(ctx: MutableCtx) -> anyhow::Result<()> {
    match query_status(ctx.storage)? {
        // If the account is inactive: only the gateway may deposit into it.
        // Reject transfers from any other sender. A sufficient deposit from
        // the gateway flips the account to `Active`.
        AccountStatus::Inactive => {
            ensure!(
                {
                    let gateway = ctx.querier.query_gateway()?;
                    ctx.sender == gateway
                },
                "account {} is not active, only the gateway can deposit into it",
                ctx.contract
            );

            // Activation is based on the account's *balance* after this
            // deposit, not the size of this single deposit. This lets a
            // sequence of sub-minimum gateway deposits accumulate until the
            // threshold is met.
            let minimum = ctx.querier.query_minimum_deposit()?;
            if has_sufficient_balance(ctx.querier, ctx.contract, &minimum)? {
                account::STATUS.save(ctx.storage, &AccountStatus::Active)?;
                // TODO: emit an event?
            }
        },
        AccountStatus::Frozen => {
            bail!(
                "account {} is frozen, can't receive transfers",
                ctx.contract
            );
        },
        AccountStatus::Active => { /* nothing to do */ },
    }

    Ok(())
}

/// A deposit is considered **sufficient** if _either_ of the following is true:
/// - the minimum deposit is zero;
/// - _any_ of the coins received has an amount greater than the minimum.
fn is_sufficient_deposit(deposit: &Coins, minimum: &Coins) -> bool {
    if minimum.is_empty() {
        return true;
    }

    for coin in minimum {
        if deposit.amount_of(coin.denom) >= *coin.amount {
            return true;
        }
    }

    false
}

/// The same as `is_sufficient_deposit`, but checks the account's total balance,
/// which can be the sum of multiple deposits.
fn has_sufficient_balance(
    querier: QuerierWrapper,
    address: Addr,
    minimum: &Coins,
) -> StdResult<bool> {
    if minimum.is_empty() {
        return Ok(true);
    }

    for coin in minimum {
        let balance = querier.query_balance(address, coin.denom.clone())?;
        if balance >= *coin.amount {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Authenticate a transaction by ensuring:
///
/// - the username is associated with the sender account;
/// - the nonce is acceptible;
/// - the signature is authentic.
///
/// This logic is used by single-signature accounts.
pub fn authenticate_tx(
    ctx: AuthCtx,
    tx: Tx,
    // The deserialized metadata, if it's already done, so we don't have to redo
    // the work here.
    maybe_metadata: Option<Metadata>,
) -> anyhow::Result<()> {
    let factory = ctx.querier.query_account_factory()?;

    // Deserialize the transaction metadata, if it's not already done.
    let metadata = if let Some(metadata) = maybe_metadata {
        metadata
    } else {
        tx.data.clone().deserialize_json()?
    };

    // Query the user's profile.
    let user = ctx
        .querier
        .query_wasm_path::<User, _>(factory, &account_factory::USERS.path(metadata.user_index))?;

    // The sender's address and the user profile declared in metadata must match.
    ensure!(
        user.accounts.values().any(|a| *a == tx.sender),
        "account {} isn't associated with user {}",
        tx.sender,
        metadata.user_index,
    );

    // The account must be in the `Active` state.
    ensure!(
        query_status(ctx.storage)? == AccountStatus::Active,
        "account {} is not active",
        tx.sender
    );

    verify_nonce_and_signature(ctx, tx, &user, Some(metadata))
}

/// Ensure the nonce is acceptable and the signature is authentic.
///
/// Compared to [`authenticate_tx`](crate::authenticate_tx), this function skips
/// the part of verifying the username.
///
/// This is intended for the multi-signature accounts, where we ensure the
/// username is associated with the multisig _at the time a proposal was created_,
/// instead of _now_.
pub fn verify_nonce_and_signature(
    ctx: AuthCtx,
    tx: Tx,
    user: &User,
    maybe_metadata: Option<Metadata>,
) -> anyhow::Result<()> {
    // Deserialize the transaction metadata, if it's not already done.
    let metadata = if let Some(metadata) = maybe_metadata {
        metadata
    } else {
        tx.data.deserialize_json()?
    };

    // Ensure the chain ID in metadata matches the context.
    ensure!(
        metadata.chain_id == ctx.chain_id,
        "chain ID mismatch: expecting `{}`, got `{}`",
        ctx.chain_id,
        metadata.chain_id
    );

    let sign_doc = SignDoc {
        gas_limit: tx.gas_limit,
        sender: ctx.contract,
        messages: tx.msgs,
        data: metadata.clone(),
    };

    match ctx.mode {
        AuthMode::Check | AuthMode::Finalize => {
            // Deserialize the credential first, so we can pick the nonce
            // namespace: standard (master-key) credentials and session keys
            // track their nonces in separate windows.
            let (
                StandardCredential {
                    key_hash,
                    signature,
                },
                session_credential,
            ) = match tx.credential.deserialize_json::<Credential>()? {
                Credential::Session(c) => (c.authorization.clone(), Some(c)),
                Credential::Standard(c) => (c, None),
            };

            // Verify and record the nonce in the appropriate window.
            match &session_credential {
                // Session credential: each session key gets its own sliding
                // window, so independent session keys (e.g. one per bot) don't
                // collide. The floor closure is evaluated only when this key's
                // window is empty (its first use).
                Some(session) => {
                    let session_key = session.session_info.session_key;

                    let mut nonces = account::SESSION_SEEN_NONCES
                        .may_load(ctx.storage, session_key)?
                        .unwrap_or_default();

                    let may_load_floor = || {
                        account::SEEN_NONCES
                            .may_load(ctx.storage)
                            .map(|opt| opt.and_then(|set| set.last().copied()))
                    };

                    check_and_record_nonce(&mut nonces, metadata.nonce, may_load_floor)?;

                    account::SESSION_SEEN_NONCES.save(ctx.storage, session_key, &nonces)?;
                },
                // Standard credential: the account-wide window (unchanged).
                None => {
                    let mut nonces = account::SEEN_NONCES
                        .may_load(ctx.storage)?
                        .unwrap_or_default();

                    check_and_record_nonce(&mut nonces, metadata.nonce, || Ok(None))?;

                    account::SEEN_NONCES.save(ctx.storage, &nonces)?;
                },
            }

            // Verify tx expiration.
            if let Some(expiry) = metadata.expiry {
                ensure!(
                    expiry > ctx.block.timestamp,
                    "transaction expired at {expiry:?}"
                );
            }

            // Query the key by key hash and user index.
            let key = user
                .keys
                .get(&key_hash)
                .copied()
                .ok_or_else(|| anyhow!("user does not have a key with hash {key_hash}"))?;

            if let Some(session) = session_credential {
                ensure!(
                    session.session_info.expire_at > ctx.block.timestamp,
                    "session expired at {:?}.",
                    session.session_info.expire_at
                );

                ensure!(
                    session.session_info.chain_id == ctx.chain_id,
                    "session chain_id mismatch: expecting `{}`, got `{}`",
                    ctx.chain_id,
                    session.session_info.chain_id
                );

                // Verify the `SessionInfo` signature.
                //
                // TODO: we can consider saving authorized session keys in the
                // contract, so it's not necessary to verify them again.
                verify_signature(
                    ctx.api,
                    key,
                    signature,
                    VerifyData::Session(session.session_info.clone()),
                )?;

                // Verify the `SignDoc` signature.
                verify_signature(
                    ctx.api,
                    Key::Secp256k1(session.session_info.session_key),
                    Signature::Secp256k1(session.session_signature),
                    VerifyData::Transaction(sign_doc),
                )?;
            } else {
                // Verify the `SignDoc` signature.
                verify_signature(ctx.api, key, signature, VerifyData::Transaction(sign_doc))?;
            }
        },
        // No need to verify nonce neither signature in simulation mode.
        AuthMode::Simulate => (),
    };

    Ok(())
}

// -------------------------- EIP-712 typed data -------------------------------
//
// We reconstruct the EIP-712 resolver, domain, and message on-chain rather than
// trusting the `typed_data` embedded in the credential. This guarantees the
// signature commits to exactly the fields we verify: a malicious or buggy
// client can't downgrade a field's type to erase its content from the signing
// hash (only `cred.sig` is consumed from the credential).
//
// EIP-712 has no sum type, so an externally tagged enum -- the `Message` enum in
// a transaction, the `Key` enum in onboarding -- can't be expressed as a struct.
// We bind each such value as its canonical JSON `string` (recursively
// key-sorted, compact), the same canonicalization the raw Secp256k1 and Passkey
// paths already sign over. `string` is an atomic EIP-712 type hashed as
// `keccak256(utf8_bytes)`, so content, variant tag, and array length are all
// bound.

/// The canonical EIP-712 resolver for a transaction [`SignDoc`].
///
/// `messages` is typed as `string[]`; each element is the canonical JSON string
/// of one message (see [`tx_eip712_message`]). `has_expiry` toggles the optional
/// `Metadata::expiry` field, which is omitted from serialization when `None`.
fn tx_eip712_resolver(has_expiry: bool) -> StdResult<Resolver> {
    let mut metadata_fields = vec![
        json!({ "name": "user_index", "type": "uint32" }),
        json!({ "name": "chain_id",   "type": "string" }),
        json!({ "name": "nonce",      "type": "uint32" }),
    ];

    if has_expiry {
        metadata_fields.push(json!({ "name": "expiry", "type": "string" }));
    }

    json!({
        "Message": [
            { "name": "sender",    "type": "address"  },
            { "name": "data",      "type": "Metadata" },
            { "name": "gas_limit", "type": "uint32"   },
            { "name": "messages",  "type": "string[]" },
        ],
        "Metadata": metadata_fields,
    })
    .deserialize_json()
}

/// The EIP-712 domain for a transaction. `verifyingContract` is the sender's
/// address.
fn tx_eip712_domain(sender: Addr) -> Eip712Domain {
    Eip712Domain {
        name: Some("dango".into()),
        // We use Ethereum's EIP-155 chainId (0x1) for compatibility.
        chain_id: Some(EIP155_CHAIN_ID),
        verifying_contract: Some(U160::from_be_bytes(sender.into_inner()).into()),
        ..Default::default()
    }
}

/// The EIP-712 `message` value for a transaction [`SignDoc`]: the same fields as
/// the SignDoc's canonical JSON, but with `messages` replaced by an array of
/// canonical JSON strings (one per message), so the signature commits to message
/// content.
fn tx_eip712_message(sign_doc: &SignDoc) -> StdResult<JsonValue> {
    let messages = sign_doc
        .messages
        .iter()
        .map(|msg| Ok(JsonValue::String(msg.to_json_value()?.to_json_string()?)))
        .collect::<StdResult<Vec<_>>>()?;

    let mut message = sign_doc.to_json_value()?.into_inner();
    if let Some(obj) = message.as_object_mut() {
        obj.insert("messages".to_string(), JsonValue::Array(messages));
    }

    Ok(message)
}

/// The EIP-712 domain for "arbitrary" payloads (session keys, onboarding),
/// matching the SDK's `composeArbitraryTypedData`: a fixed name and the zero
/// address as `verifyingContract`, since these payloads aren't bound to a
/// specific contract.
fn arbitrary_eip712_domain() -> Eip712Domain {
    Eip712Domain {
        name: Some("DangoArbitraryMessage".into()),
        chain_id: Some(EIP155_CHAIN_ID),
        verifying_contract: Some(address!("0x0000000000000000000000000000000000000000")),
        ..Default::default()
    }
}

/// The canonical EIP-712 resolver for a [`SessionInfo`]. All fields are atomic
/// strings, so no special encoding is needed.
fn session_eip712_resolver() -> StdResult<Resolver> {
    json!({
        "Message": [
            { "name": "chain_id",    "type": "string" },
            { "name": "expire_at",   "type": "string" },
            { "name": "session_key", "type": "string" },
        ],
    })
    .deserialize_json()
}

/// The canonical EIP-712 resolver for a [`RegisterUserData`]. `key` is typed as
/// `string` because `Key` is an enum EIP-712 can't express as a struct (see
/// [`onboard_eip712_message`]); `has_referrer` toggles the optional `referrer`
/// field, omitted from serialization when `None`.
fn onboard_eip712_resolver(has_referrer: bool) -> StdResult<Resolver> {
    let mut fields = vec![
        json!({ "name": "chain_id", "type": "string" }),
        json!({ "name": "key",      "type": "string" }),
        json!({ "name": "key_hash", "type": "string" }),
    ];

    // Field order must match the order the client signs (the EIP-712 type hash
    // is order-sensitive). The client lists fields alphabetically, so the
    // optional `referrer` (omitted from serialization when `None`) precedes
    // `seed`.
    if has_referrer {
        fields.push(json!({ "name": "referrer", "type": "uint32" }));
    }

    fields.push(json!({ "name": "seed", "type": "uint32" }));

    json!({ "Message": fields }).deserialize_json()
}

/// The EIP-712 `message` value for a [`RegisterUserData`]: the same fields as its
/// canonical JSON, but with `key` replaced by the canonical JSON string of the
/// `Key` enum, so the signature commits to the key (as the struct's doc comment
/// intends).
fn onboard_eip712_message(onboard: &RegisterUserData) -> StdResult<JsonValue> {
    let key = JsonValue::String(onboard.key.to_json_value()?.to_json_string()?);

    let mut message = onboard.to_json_value()?.into_inner();
    if let Some(obj) = message.as_object_mut() {
        obj.insert("key".to_string(), key);
    }

    Ok(message)
}

/// Reconstruct the full EIP-712 typed data for a signed payload.
///
/// This is the single source of truth shared by the on-chain verifier
/// ([`verify_signature`]) and the Rust SDK's `Eip712` signer, so a signature
/// always commits to exactly the fields the chain re-derives and checks. The
/// `typed_data` a client embeds in its credential is never trusted.
pub fn build_eip712_typed_data(data: &VerifyData) -> StdResult<TypedData> {
    let (resolver, domain, message) = match data {
        VerifyData::Transaction(sign_doc) => (
            tx_eip712_resolver(sign_doc.data.expiry.is_some())?,
            tx_eip712_domain(sign_doc.sender),
            tx_eip712_message(sign_doc)?,
        ),
        VerifyData::Session(session_info) => (
            session_eip712_resolver()?,
            arbitrary_eip712_domain(),
            session_info.to_json_value()?.into_inner(),
        ),
        VerifyData::Onboard(onboard) => (
            onboard_eip712_resolver(onboard.referrer.is_some())?,
            arbitrary_eip712_domain(),
            onboard_eip712_message(onboard)?,
        ),
    };

    Ok(TypedData {
        resolver,
        domain,
        primary_type: "Message".to_string(),
        message,
    })
}

pub fn verify_signature(
    api: &dyn Api,
    key: Key,
    signature: Signature,
    data: VerifyData,
) -> anyhow::Result<()> {
    match (key, signature) {
        (Key::Ethereum(addr), Signature::Eip712(cred)) => {
            // Reconstruct the typed data locally rather than trusting the
            // `typed_data` embedded in the credential; only `cred.sig` is
            // consumed. See `build_eip712_typed_data`.
            let sign_bytes = build_eip712_typed_data(&data)?.eip712_signing_hash()?;

            // The first 64 bytes of the Ethereum signature is the typical
            // Secp256k1 signature, while the last byte is the recovery ID.
            //
            // In Ethereum, the recovery ID is usually 27 or 28, instead of the
            // standard 0 or 1 used in raw ECDSA recoverable signatures.
            // However, our `Api` implementation should handle this, so no action
            // is needed here.
            let signature = &cred.sig[0..64];
            let recovery_id = cred.sig[64];

            // Recover the Ethereum public key from the signature.
            let pk = api.secp256k1_pubkey_recover(&sign_bytes.0, signature, recovery_id, false)?;

            // Derive Ethereum address from the public key.
            let recovered_addr = &api.keccak256(&pk[1..])[12..];

            ensure!(
                addr.as_ref() == recovered_addr,
                "recovered Ethereum address does not match: {} != {}",
                addr,
                hex::encode(recovered_addr)
            );
        },
        (Key::Secp256r1(pk), Signature::Passkey(cred)) => {
            let signed_hash = {
                let client_data: ClientData = cred.client_data.deserialize_json()?;

                let sign_data = data.to_sign_data()?;
                let sign_data_base64 = BASE64URL_NOPAD.encode(&sign_data);

                ensure!(
                    client_data.challenge == sign_data_base64,
                    "incorrect challenge: expecting {}, got {}",
                    sign_data_base64,
                    client_data.challenge
                );

                let client_data_hash = api.sha2_256(&cred.client_data);

                let signed_data = [
                    cred.authenticator_data.as_ref(),
                    client_data_hash.as_slice(),
                ]
                .concat();

                // Note we use the FFI `sha2_256` method instead of `hash256`
                // from `HashExt`, because we may change the hash function
                // used in `HashExt` (we're exploring BLAKE3 over SHA-256).
                // Passkey always signs over SHA-256 digests.
                api.sha2_256(&signed_data)
            };

            api.secp256r1_verify(&signed_hash, &cred.sig, &pk)?;
        },
        (Key::Secp256k1(pk), Signature::Secp256k1(sig)) => {
            let sign_data = data.to_sign_data()?;

            api.secp256k1_verify(&sign_data, &sig, &pk)?;
        },
        _ => bail!("key and credential types don't match!"),
    }
    Ok(())
}

/// The type of data that was signed.
pub enum VerifyData {
    /// The signature is for sending a transaction.
    ///
    /// To do this, the user must sign a `SignDoc` using either their primary
    /// key or a session key that has been authorized.
    Transaction(SignDoc),
    /// The signature is for authorizing a session key to send transactions
    /// on behalf on the primary key.
    ///
    /// To do this, the user must sign a `SessionInfo`.
    Session(SessionInfo),
    /// The signature is for onboarding a new user.
    ///
    /// To do this, the user must sign a `RegisterUserData`.
    Onboard(RegisterUserData),
}

impl SignData for VerifyData {
    type Error = StdError;
    type Hasher = Sha256;

    fn to_prehash_sign_data(&self) -> StdResult<Vec<u8>> {
        match self {
            VerifyData::Transaction(sign_doc) => sign_doc.to_prehash_sign_data(),
            VerifyData::Session(session_info) => session_info.to_prehash_sign_data(),
            VerifyData::Onboard(data) => data.to_prehash_sign_data(),
        }
    }
}

// ----------------------------------- tests -----------------------------------

/// Tests below use hardcoded cryptographic fixtures (signatures, typed data
/// blobs, etc.) that are tied to the exact shape of the signed structs
/// (`SignDoc`, `SessionInfo`, `RegisterUserData`).
///
/// If you change any of these structs, the fixtures must be regenerated:
///
/// ```bash
/// cargo run --example generate_test_data -p dango-auth
/// ```
///
/// Then paste the output into the corresponding test functions.
#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::account_factory::USERS,
        dango_primitives::{
            Addr, AuthMode, Hash256, MockContext, MockQuerier, MockStorage, ResultExt, addr,
            btree_map, hash,
        },
        dango_types::{
            account_factory::Username,
            config::{AppAddresses, AppConfig},
        },
        hex_literal::hex,
        std::str::FromStr,
    };

    /// Address of the account factory for use in the following tests.
    const ACCOUNT_FACTORY: Addr = Addr::mock(254);

    #[test]
    fn passkey_authentication() {
        let user_address = Addr::from_str("0xd7b73f486c66fa6daecd67d7aee46a26513b07c2").unwrap();
        let user_index = 123;
        let user_keyhash =
            Hash256::from_str("244EA558C35EF9521EBA7418B72C94395235D678C6BDDD934EE514A6BC097FD8")
                .unwrap();
        let user_key = Key::Secp256r1(
            [
                2, 69, 17, 109, 179, 224, 216, 88, 134, 155, 142, 29, 222, 224, 160, 235, 116, 12,
                211, 16, 191, 65, 88, 180, 255, 202, 173, 80, 196, 146, 44, 111, 119,
            ]
            .into(),
        );

        let tx = r#"{
          "sender": "0xd7b73f486c66fa6daecd67d7aee46a26513b07c2",
          "credential": {
            "standard": {
              "signature": {
                "passkey": {
                  "sig": "L/ne0uoF3/aI73itjvcvW2AZ6fAJEd+QNSj/juzJc1zP9EeA++42ilmW03kJWlcqQKxTWaZQlEWCrdCnCnXU+A==",
                  "client_data": "eyJ0eXBlIjoid2ViYXV0aG4uZ2V0IiwiY2hhbGxlbmdlIjoiZ2lzQzkzblFTUWRzOVo2WEp6X0xEQXZOdHN0b3k2b091SERhMEl3ZllqcyIsIm9yaWdpbiI6Imh0dHA6Ly9sb2NhbGhvc3Q6NTA4MCIsImNyb3NzT3JpZ2luIjpmYWxzZX0=",
                  "authenticator_data": "SZYN5YgOjGh0NBcPZHZgW4/krrmihjLHmVzzuoMdl2MZAAAAAA=="
                }
              },
              "key_hash": "244EA558C35EF9521EBA7418B72C94395235D678C6BDDD934EE514A6BC097FD8"
            }
          },
          "data": {
            "chain_id": "dev-6",
            "user_index": 123,
            "nonce": 0
          },
          "msgs": [
            {
              "transfer": {
                "0x33361de42571d6aa20c37daa6da4b5ab67bfaad9": {
                  "bridge/usdc": "1000000"
                }
              }
            }
          ],
          "gas_limit": 2834
        }"#;

        let mut storage = MockStorage::new();

        account::STATUS
            .save(&mut storage, &AccountStatus::Active)
            .unwrap();

        let querier = MockQuerier::new()
            .with_app_config(AppConfig {
                addresses: AppAddresses {
                    account_factory: ACCOUNT_FACTORY,
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap()
            .with_raw_contract_storage(ACCOUNT_FACTORY, |storage| {
                let user = User {
                    index: user_index,
                    name: Username::default_for_index(user_index),
                    accounts: btree_map! { 0u32 => user_address },
                    keys: btree_map! { user_keyhash => user_key },
                };
                USERS.save(storage, user_index, &user).unwrap();
            });

        let mut ctx = MockContext::new()
            .with_storage(storage)
            .with_querier(querier)
            .with_contract(user_address)
            .with_chain_id("dev-6")
            .with_mode(AuthMode::Finalize);

        authenticate_tx(ctx.as_auth(), tx.deserialize_json().unwrap(), None).should_succeed();
    }

    #[test]
    fn eip712_authentication() {
        let user_address = Addr::from_str("0xe928dcd380a863f74d7e8867ac7908a296b4b2e1").unwrap();
        let user_index = 2410702975;
        let user_keyhash =
            Hash256::from_str("9CB20F8AB2252CFDD6636DB34942D8C94E85EF5B6E414318DD05B9B191DABDA6")
                .unwrap();
        let user_key =
            Key::Ethereum(Addr::from_str("0x90208cb1baef0e1922a50c6952b064a2f0993e73").unwrap());

        let mut storage = MockStorage::new();

        account::STATUS
            .save(&mut storage, &AccountStatus::Active)
            .unwrap();

        let querier = MockQuerier::new()
            .with_app_config(AppConfig {
                addresses: AppAddresses {
                    account_factory: ACCOUNT_FACTORY,
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap()
            .with_raw_contract_storage(ACCOUNT_FACTORY, |storage| {
                let user = User {
                    index: user_index,
                    name: Username::default_for_index(user_index),
                    accounts: btree_map! { 0u32 => user_address },
                    keys: btree_map! { user_keyhash => user_key },
                };
                USERS.save(storage, user_index, &user).unwrap();
            });

        let mut ctx = MockContext::new()
            .with_storage(storage)
            .with_querier(querier)
            .with_contract(user_address)
            .with_chain_id("dev-1")
            .with_mode(AuthMode::Finalize);

        let tx = r#"{
          "sender": "0xe928dcd380a863f74d7e8867ac7908a296b4b2e1",
          "credential": {
            "standard": {
              "signature": {
                "eip712": {
                  "sig": "xxuTM6yhJMLev9DdxKN3mBakwa4+g2cFlJMv6JI/dp8SGq2r5FwTdwEeapg7mNdmkN8ZfppHLqc/fi9nxn1h0Bs=",
                  "typed_data": "eyJkb21haW4iOnsibmFtZSI6ImRhbmdvIiwiY2hhaW5JZCI6IjB4MSIsInZlcmlmeWluZ0NvbnRyYWN0IjoiMHhlOTI4ZGNkMzgwYTg2M2Y3NGQ3ZTg4NjdhYzc5MDhhMjk2YjRiMmUxIn0sInR5cGVzIjp7Ik1lc3NhZ2UiOlt7InR5cGUiOiJhZGRyZXNzIiwibmFtZSI6InNlbmRlciJ9LHsidHlwZSI6Ik1ldGFkYXRhIiwibmFtZSI6ImRhdGEifSx7InR5cGUiOiJ1aW50MzIiLCJuYW1lIjoiZ2FzX2xpbWl0In0seyJ0eXBlIjoic3RyaW5nW10iLCJuYW1lIjoibWVzc2FnZXMifV0sIk1ldGFkYXRhIjpbeyJ0eXBlIjoidWludDMyIiwibmFtZSI6InVzZXJfaW5kZXgifSx7InR5cGUiOiJzdHJpbmciLCJuYW1lIjoiY2hhaW5faWQifSx7InR5cGUiOiJ1aW50MzIiLCJuYW1lIjoibm9uY2UifV19LCJwcmltYXJ5VHlwZSI6Ik1lc3NhZ2UiLCJtZXNzYWdlIjp7ImRhdGEiOnsiY2hhaW5faWQiOiJkZXYtMSIsIm5vbmNlIjo4NSwidXNlcl9pbmRleCI6MjQxMDcwMjk3NX0sImdhc19saW1pdCI6NDEwOTc5ODMzNywibWVzc2FnZXMiOlsie1widHJhbnNmZXJcIjp7XCIweGNhYzFhYjUwZmI3ZjE4ODkyNGM5MWQxY2JlNjgxNTYwNTMyMGQ2MTlcIjp7XCJicmlkZ2UvdXNkY1wiOlwiMTAwMDAwMDAwXCJ9fX0iXSwic2VuZGVyIjoiMHhlOTI4ZGNkMzgwYTg2M2Y3NGQ3ZTg4NjdhYzc5MDhhMjk2YjRiMmUxIn19"
                }
              },
              "key_hash": "9CB20F8AB2252CFDD6636DB34942D8C94E85EF5B6E414318DD05B9B191DABDA6"
            }
          },
          "data": {
            "chain_id": "dev-1",
            "user_index": 2410702975,
            "nonce": 85
          },
          "msgs": [
            {
              "transfer": {
                "0xcac1ab50fb7f188924c91d1cbe6815605320d619": {
                  "bridge/usdc": "100000000"
                }
              }
            }
          ],
          "gas_limit": 4109798337
        }"#;

        authenticate_tx(ctx.as_auth(), tx.deserialize_json::<Tx>().unwrap(), None).should_succeed();
    }

    #[test]
    fn secp256k1_authentication() {
        let user_address = addr!("843a9778a711d5474ef6efd65fca38731f281471");
        let user_index = 231893934;
        let user_keyhash =
            hash!("94eb754d36ed86af6fc231eae13c78b4a298ed065f63eb8dac139b8b943b76da");
        let user_key = Key::Secp256k1(
            hex!("022e730b2e26c6e3ce78d28c3700da0a798d893a73ee15055baaaee1cf46db7a4a").into(),
        );

        let tx = r#"{
          "sender": "0x843a9778a711d5474ef6efd65fca38731f281471",
          "gas_limit": 7378592558624777017,
          "msgs": [
            {
              "transfer": {
                "0x836ca678a5afe736c6b64b2d5a6ee4bc85588cd8": {
                  "bridge/usdc": "100000000"
                }
              }
            }
          ],
          "data": {
            "chain_id": "dev-1",
            "nonce": 27,
            "user_index": 231893934
          },
          "credential": {
            "standard": {
              "key_hash": "94EB754D36ED86AF6FC231EAE13C78B4A298ED065F63EB8DAC139B8B943B76DA",
              "signature": {
                "secp256k1": "LEohIzCuV3/MRLM/XvZNcxUdNp/Q811IsioZ3SEBbbRMvXlrvWi1v3+NYeZBYnALVtQzZcpO1E2wqiBd64lSdg=="
              }
            }
          }
        }"#;

        let mut storage = MockStorage::new();

        account::STATUS
            .save(&mut storage, &AccountStatus::Active)
            .unwrap();

        let querier = MockQuerier::new()
            .with_app_config(AppConfig {
                addresses: AppAddresses {
                    account_factory: ACCOUNT_FACTORY,
                    // Address below don't matter for this test.
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap()
            .with_raw_contract_storage(ACCOUNT_FACTORY, |storage| {
                let user = User {
                    index: user_index,
                    name: Username::default_for_index(user_index),
                    accounts: btree_map! { 0u32 => user_address },
                    keys: btree_map! { user_keyhash => user_key },
                };
                USERS.save(storage, user_index, &user).unwrap();
            });

        // With account in the `Inactive` state. Should fail.
        let mut ctx = MockContext::new()
            .with_storage(MockStorage::default()) // use the default storage, which doesn't contain the active status flag
            .with_querier(querier)
            .with_contract(user_address)
            .with_chain_id("not-dev-1")
            .with_mode(AuthMode::Finalize);

        authenticate_tx(ctx.as_auth(), tx.deserialize_json().unwrap(), None)
            .should_fail_with_error("is not active");

        // With the incorrect chain ID. Should fail.
        let mut ctx = MockContext::new()
            .with_storage(storage) // use the storage that contains the active status flag
            .with_querier(ctx.querier)
            .with_contract(user_address)
            .with_chain_id("not-dev-1")
            .with_mode(AuthMode::Finalize);

        authenticate_tx(ctx.as_auth(), tx.deserialize_json().unwrap(), None)
            .should_fail_with_error("chain ID mismatch");

        // With the correct chain ID.
        let mut ctx = MockContext::new()
            .with_storage(ctx.storage)
            .with_querier(ctx.querier)
            .with_contract(user_address)
            .with_chain_id("dev-1")
            .with_mode(AuthMode::Finalize);

        authenticate_tx(ctx.as_auth(), tx.deserialize_json().unwrap(), None).should_succeed();
    }

    #[test]
    fn session_key_with_passkey_authentication() {
        let user_address = Addr::from_str("0xab2c9227569959eaa46b86c20eb2c3bcbb1c8873").unwrap();
        let user_index = 558063273;
        let user_keyhash =
            Hash256::from_str("5A014F459EC3D7EBC13904B7DCB3BFD4A923A7943F49ED435637C7AA16DF4F88")
                .unwrap();
        let user_key = Key::Secp256r1(
            [
                2, 162, 95, 0, 60, 251, 195, 142, 6, 181, 226, 73, 162, 201, 50, 187, 102, 19, 163,
                124, 96, 77, 19, 229, 197, 127, 146, 195, 177, 180, 186, 38, 243,
            ]
            .into(),
        );

        let mut storage = MockStorage::new();

        account::STATUS
            .save(&mut storage, &AccountStatus::Active)
            .unwrap();

        let querier = MockQuerier::new()
            .with_app_config(AppConfig {
                addresses: AppAddresses {
                    account_factory: ACCOUNT_FACTORY,
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap()
            .with_raw_contract_storage(ACCOUNT_FACTORY, |storage| {
                let user = User {
                    index: user_index,
                    name: Username::default_for_index(user_index),
                    accounts: btree_map! { 0u32 => user_address },
                    keys: btree_map! { user_keyhash => user_key },
                };
                USERS.save(storage, user_index, &user).unwrap();
            });

        let mut ctx = MockContext::new()
            .with_storage(storage)
            .with_querier(querier)
            .with_contract(user_address)
            .with_chain_id("dev-1")
            .with_mode(AuthMode::Finalize);

        let tx = r#"{
          "sender": "0xab2c9227569959eaa46b86c20eb2c3bcbb1c8873",
          "credential": {
            "session": {
              "authorization": {
                "key_hash": "5A014F459EC3D7EBC13904B7DCB3BFD4A923A7943F49ED435637C7AA16DF4F88",
                "signature": {
                  "passkey": {
                    "authenticator_data": "SZYN5YgOjGh0NBcPZHZgW4/krrmihjLHmVzzuoMdl2MZAAAAAA==",
                    "client_data": "eyJ0eXBlIjoid2ViYXV0aG4uZ2V0IiwiY2hhbGxlbmdlIjoiYTFCT3IyYUlNWXVQb3pma2VXYzRiXzJrSGIxcE5jQUlPTXQ2djFOZlNMbyIsIm9yaWdpbiI6Imh0dHA6Ly9sb2NhbGhvc3Q6NTA4MCIsImNyb3NzT3JpZ2luIjpmYWxzZX0=",
                    "sig": "VWUJvzgahil1C98SrwoD0Wg0p+hEJaLggqeTqIJ45lY8CWDXXxfr8aJ2ArPAFjBLGTF2oG1G7pMRchPFSHEMnA=="
                  }
                }
              },
              "session_info": {
                "chain_id": "dev-1",
                "expire_at": "340282366920938463463374607431.768211455",
                "session_key": "AmjeBj515CzO/hI/6bA2NPtENa/XgT3Hm+v9X4JS0ckd"
              },
              "session_signature": "e7OS4CMwKGFRUv70wreE1b/dAEaMvGfe77TDYsnAiMtRE6m/f3DcQ2IvcBIAZB0zB/EF71+U0YEiWsvu3ofiQQ=="
            }
          },
          "data": {
            "chain_id": "dev-1",
            "nonce": 33,
            "user_index": 558063273
          },
          "msgs": [
            {
              "transfer": {
                "0xd7639196e1b8156f4682c6d905dabf7b6acf3cee": {
                  "bridge/usdc": "100000000"
                }
              }
            }
          ],
          "gas_limit": 16132801695428362404
        }"#;

        authenticate_tx(ctx.as_auth(), tx.deserialize_json::<Tx>().unwrap(), None).should_succeed();
    }

    #[test]
    fn session_key_with_eip712_authentication() {
        let user_address = Addr::from_str("0x95a85fe292991bfa52f81a15292f758cbc26669e").unwrap();
        let user_index = 3253918834;
        let user_keyhash =
            Hash256::from_str("802C3DF10B0B24A63CD9B3B1D70B00D1574F04D9EE2C9DB1BAEBB2444579A204")
                .unwrap();
        let user_key =
            Key::Ethereum(Addr::from_str("0x528b4cbc3c8f954b5aede2b90b5c69c796360e53").unwrap());

        let mut storage = MockStorage::new();

        account::STATUS
            .save(&mut storage, &AccountStatus::Active)
            .unwrap();

        let querier = MockQuerier::new()
            .with_app_config(AppConfig {
                addresses: AppAddresses {
                    account_factory: ACCOUNT_FACTORY,
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap()
            .with_raw_contract_storage(ACCOUNT_FACTORY, |storage| {
                let user = User {
                    index: user_index,
                    name: Username::default_for_index(user_index),
                    accounts: btree_map! { 0u32 => user_address },
                    keys: btree_map! { user_keyhash => user_key },
                };
                USERS.save(storage, user_index, &user).unwrap();
            });

        let mut ctx = MockContext::new()
            .with_storage(storage)
            .with_querier(querier)
            .with_contract(user_address)
            .with_chain_id("dev-1")
            .with_mode(AuthMode::Finalize);

        let tx = r#"{
          "sender": "0x95a85fe292991bfa52f81a15292f758cbc26669e",
          "credential": {
            "session": {
              "authorization": {
                "key_hash": "802C3DF10B0B24A63CD9B3B1D70B00D1574F04D9EE2C9DB1BAEBB2444579A204",
                "signature": {
                  "eip712": {
                    "sig": "XZwKBnSP7AAWmLxWoEdnbkyGfbifT8eoFTc5ZMpEGTpHzsGugrRPLF+mNQxWWNT0aMQ9cRW1Wy0pgTFWmEHvZBw=",
                    "typed_data": "eyJkb21haW4iOnsiY2hhaW5JZCI6MSwibmFtZSI6IkRhbmdvQXJiaXRyYXJ5TWVzc2FnZSIsInZlcmlmeWluZ0NvbnRyYWN0IjoiMHgwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwIn0sIm1lc3NhZ2UiOnsiY2hhaW5faWQiOiJkZXYtMSIsImV4cGlyZV9hdCI6IjM0MDI4MjM2NjkyMDkzODQ2MzQ2MzM3NDYwNzQzMS43NjgyMTE0NTUiLCJzZXNzaW9uX2tleSI6IkFnYU5Va1kzdUdEdXFqR2puam8xRXFTb205RnFQaHArN3R2dld4NzVtRkVuIn0sInByaW1hcnlUeXBlIjoiTWVzc2FnZSIsInR5cGVzIjp7IkVJUDcxMkRvbWFpbiI6W3sibmFtZSI6Im5hbWUiLCJ0eXBlIjoic3RyaW5nIn0seyJuYW1lIjoiY2hhaW5JZCIsInR5cGUiOiJ1aW50MjU2In0seyJuYW1lIjoidmVyaWZ5aW5nQ29udHJhY3QiLCJ0eXBlIjoiYWRkcmVzcyJ9XSwiTWVzc2FnZSI6W3sibmFtZSI6ImNoYWluX2lkIiwidHlwZSI6InN0cmluZyJ9LHsibmFtZSI6ImV4cGlyZV9hdCIsInR5cGUiOiJzdHJpbmcifSx7Im5hbWUiOiJzZXNzaW9uX2tleSIsInR5cGUiOiJzdHJpbmcifV19fQ=="
                  }
                }
              },
              "session_info": {
                "chain_id": "dev-1",
                "expire_at": "340282366920938463463374607431.768211455",
                "session_key": "AgaNUkY3uGDuqjGjnjo1EqSom9FqPhp+7tvvWx75mFEn"
              },
              "session_signature": "/1+xBGwj+eXQ3/u8kPTVcEbgJrQ2unUwg1Bl5+d6bZQOsgemwzSmNm8lQMZaaePW1h7MCt3AQSfyirWW5F2/ZQ=="
            }
          },
          "data": {
            "chain_id": "dev-1",
            "nonce": 40,
            "user_index": 3253918834
          },
          "msgs": [
            {
              "transfer": {
                "0x531806a49f59bf49f2eea445fe45aaee32eeca4d": {
                  "bridge/usdc": "100000000"
                }
              }
            }
          ],
          "gas_limit": 1324321884761996338
        }"#;

        authenticate_tx(ctx.as_auth(), tx.deserialize_json::<Tx>().unwrap(), None).should_succeed();
    }

    #[test]
    fn session_key_with_secp256k1_authentication() {
        let user_address = Addr::from_str("0xa8a31f92f5895050b9a48f9f82a1192054e9e59d").unwrap();
        let user_index = 3348916482;
        let user_keyhash =
            Hash256::from_str("A4F2CFCA9B9DE01FF4E8AD3B61FA5EBCC04B680720937362547FB9CCBEEE9DB1")
                .unwrap();
        let user_key = Key::Secp256k1(
            hex!("03e4a017d370744f3ff60084ba7d2e96714186ee94c1a83c0ac07274ce9dc56825").into(),
        );

        let mut storage = MockStorage::new();

        account::STATUS
            .save(&mut storage, &AccountStatus::Active)
            .unwrap();

        let querier = MockQuerier::new()
            .with_app_config(AppConfig {
                addresses: AppAddresses {
                    account_factory: ACCOUNT_FACTORY,
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap()
            .with_raw_contract_storage(ACCOUNT_FACTORY, |storage| {
                let user = User {
                    index: user_index,
                    name: Username::default_for_index(user_index),
                    accounts: btree_map! { 0u32 => user_address },
                    keys: btree_map! { user_keyhash => user_key },
                };
                USERS.save(storage, user_index, &user).unwrap();
            });

        let mut ctx = MockContext::new()
            .with_storage(storage)
            .with_querier(querier)
            .with_contract(user_address)
            .with_chain_id("dev-1")
            .with_mode(AuthMode::Finalize);

        let tx = r#"{
          "sender": "0xa8a31f92f5895050b9a48f9f82a1192054e9e59d",
          "credential": {
            "session": {
              "authorization": {
                "key_hash": "A4F2CFCA9B9DE01FF4E8AD3B61FA5EBCC04B680720937362547FB9CCBEEE9DB1",
                "signature": {
                  "secp256k1": "gnb5QRTYT4uMzB5LwznXA3afdTIq+cBkzD79j/jwfpZGQHEDFEZUxGtOI0YFPsv5ux2ebqMhBFnKDd0Ui8aN+Q=="
                }
              },
              "session_info": {
                "chain_id": "dev-1",
                "expire_at": "340282366920938463463374607431.768211455",
                "session_key": "A+ySwgbcVSz4l7rvzXO99RUmVvr7TVMHhHKSxnglVOQF"
              },
              "session_signature": "FtBeEXcnTR+2qERjZCKFn1eBHKHcMtzCPBdOtHgaKn4AP4Qb4bsioDk5wK+6Luz9YYW0Zq61f5tSw6ZaxLtEnw=="
            }
          },
          "data": {
            "chain_id": "dev-1",
            "nonce": 87,
            "user_index": 3348916482
          },
          "msgs": [
            {
              "transfer": {
                "0x71434023c157ca7dc649d55464e440fcf8073d8d": {
                  "bridge/usdc": "100000000"
                }
              }
            }
          ],
          "gas_limit": 3167461209082021925
        }"#;

        authenticate_tx(ctx.as_auth(), tx.deserialize_json::<Tx>().unwrap(), None).should_succeed();
    }

    #[test]
    fn authenticate_onboarding_eip712() {
        let user_key =
            Key::Ethereum(Addr::from_str("0x7ce7cb5778615e7aa678fc54a85a2bb1e8cc59b6").unwrap());
        let key_hash =
            Hash256::from_str("FFB61E9328164552FFDD3778D3F6687948B7F406AC0DD71455E44D4E30937C90")
                .unwrap();

        let mut storage = MockStorage::new();

        account::STATUS
            .save(&mut storage, &AccountStatus::Active)
            .unwrap();

        let ctx = MockContext::new()
            .with_storage(storage)
            .with_chain_id("dev-1")
            .with_mode(AuthMode::Finalize);

        let signature = r#"{
          "eip712": {
            "typed_data": "eyJkb21haW4iOnsibmFtZSI6IkRhbmdvQXJiaXRyYXJ5TWVzc2FnZSIsImNoYWluSWQiOiIweDEiLCJ2ZXJpZnlpbmdDb250cmFjdCI6IjB4MDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMCJ9LCJ0eXBlcyI6eyJNZXNzYWdlIjpbeyJ0eXBlIjoic3RyaW5nIiwibmFtZSI6ImNoYWluX2lkIn0seyJ0eXBlIjoic3RyaW5nIiwibmFtZSI6ImtleSJ9LHsidHlwZSI6InN0cmluZyIsIm5hbWUiOiJrZXlfaGFzaCJ9LHsidHlwZSI6InVpbnQzMiIsIm5hbWUiOiJzZWVkIn1dfSwicHJpbWFyeVR5cGUiOiJNZXNzYWdlIiwibWVzc2FnZSI6eyJjaGFpbl9pZCI6ImRldi0xIiwia2V5Ijoie1wiZXRoZXJldW1cIjpcIjB4N2NlN2NiNTc3ODYxNWU3YWE2NzhmYzU0YTg1YTJiYjFlOGNjNTliNlwifSIsImtleV9oYXNoIjoiRkZCNjFFOTMyODE2NDU1MkZGREQzNzc4RDNGNjY4Nzk0OEI3RjQwNkFDMERENzE0NTVFNDRENEUzMDkzN0M5MCIsInNlZWQiOjB9fQ==",
            "sig": "Y+0mZoaBHzuZlK1UVhGXSlShY5hHNH5IWejTAwLmAi8XnjjGmovD58wo80pLpJo/qVZfpB+PDvBHlR40/M//CBw="
          }
        }"#.deserialize_json::<Signature>().unwrap();

        verify_signature(
            &ctx.api,
            user_key,
            signature,
            VerifyData::Onboard(RegisterUserData {
                chain_id: "dev-1".into(),
                key: user_key,
                key_hash,
                seed: 0,
                referrer: None,
            }),
        )
        .should_succeed();
    }

    #[test]
    fn eip712_rejects_tampered_message() {
        // The valid fixture from `eip712_authentication`, but with the transfer
        // amount changed from 100000000 to 999999999 while the credential (sig +
        // typed_data) is left untouched. Because the chain now reconstructs the
        // EIP-712 hash from the actual `msgs` -- binding message content -- the
        // recovered signer no longer matches and authentication fails. Before
        // this change, message content wasn't bound and the tampered tx would
        // have authenticated.
        let user_address = Addr::from_str("0xe928dcd380a863f74d7e8867ac7908a296b4b2e1").unwrap();
        let user_index = 2410702975;
        let user_keyhash =
            Hash256::from_str("9CB20F8AB2252CFDD6636DB34942D8C94E85EF5B6E414318DD05B9B191DABDA6")
                .unwrap();
        let user_key =
            Key::Ethereum(Addr::from_str("0x90208cb1baef0e1922a50c6952b064a2f0993e73").unwrap());

        let mut storage = MockStorage::new();

        account::STATUS
            .save(&mut storage, &AccountStatus::Active)
            .unwrap();

        let querier = MockQuerier::new()
            .with_app_config(AppConfig {
                addresses: AppAddresses {
                    account_factory: ACCOUNT_FACTORY,
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap()
            .with_raw_contract_storage(ACCOUNT_FACTORY, |storage| {
                let user = User {
                    index: user_index,
                    name: Username::default_for_index(user_index),
                    accounts: btree_map! { 0u32 => user_address },
                    keys: btree_map! { user_keyhash => user_key },
                };
                USERS.save(storage, user_index, &user).unwrap();
            });

        let mut ctx = MockContext::new()
            .with_storage(storage)
            .with_querier(querier)
            .with_contract(user_address)
            .with_chain_id("dev-1")
            .with_mode(AuthMode::Finalize);

        // `bridge/usdc` amount tampered 100000000 -> 999999999; the `eip712`
        // credential is the untouched one from `eip712_authentication`.
        let tx = r#"{
          "sender": "0xe928dcd380a863f74d7e8867ac7908a296b4b2e1",
          "credential": {
            "standard": {
              "signature": {
                "eip712": {
                  "sig": "xxuTM6yhJMLev9DdxKN3mBakwa4+g2cFlJMv6JI/dp8SGq2r5FwTdwEeapg7mNdmkN8ZfppHLqc/fi9nxn1h0Bs=",
                  "typed_data": "eyJkb21haW4iOnsibmFtZSI6ImRhbmdvIiwiY2hhaW5JZCI6IjB4MSIsInZlcmlmeWluZ0NvbnRyYWN0IjoiMHhlOTI4ZGNkMzgwYTg2M2Y3NGQ3ZTg4NjdhYzc5MDhhMjk2YjRiMmUxIn0sInR5cGVzIjp7Ik1lc3NhZ2UiOlt7InR5cGUiOiJhZGRyZXNzIiwibmFtZSI6InNlbmRlciJ9LHsidHlwZSI6Ik1ldGFkYXRhIiwibmFtZSI6ImRhdGEifSx7InR5cGUiOiJ1aW50MzIiLCJuYW1lIjoiZ2FzX2xpbWl0In0seyJ0eXBlIjoic3RyaW5nW10iLCJuYW1lIjoibWVzc2FnZXMifV0sIk1ldGFkYXRhIjpbeyJ0eXBlIjoidWludDMyIiwibmFtZSI6InVzZXJfaW5kZXgifSx7InR5cGUiOiJzdHJpbmciLCJuYW1lIjoiY2hhaW5faWQifSx7InR5cGUiOiJ1aW50MzIiLCJuYW1lIjoibm9uY2UifV19LCJwcmltYXJ5VHlwZSI6Ik1lc3NhZ2UiLCJtZXNzYWdlIjp7ImRhdGEiOnsiY2hhaW5faWQiOiJkZXYtMSIsIm5vbmNlIjo4NSwidXNlcl9pbmRleCI6MjQxMDcwMjk3NX0sImdhc19saW1pdCI6NDEwOTc5ODMzNywibWVzc2FnZXMiOlsie1widHJhbnNmZXJcIjp7XCIweGNhYzFhYjUwZmI3ZjE4ODkyNGM5MWQxY2JlNjgxNTYwNTMyMGQ2MTlcIjp7XCJicmlkZ2UvdXNkY1wiOlwiMTAwMDAwMDAwXCJ9fX0iXSwic2VuZGVyIjoiMHhlOTI4ZGNkMzgwYTg2M2Y3NGQ3ZTg4NjdhYzc5MDhhMjk2YjRiMmUxIn19"
                }
              },
              "key_hash": "9CB20F8AB2252CFDD6636DB34942D8C94E85EF5B6E414318DD05B9B191DABDA6"
            }
          },
          "data": {
            "chain_id": "dev-1",
            "user_index": 2410702975,
            "nonce": 85
          },
          "msgs": [
            {
              "transfer": {
                "0xcac1ab50fb7f188924c91d1cbe6815605320d619": {
                  "bridge/usdc": "999999999"
                }
              }
            }
          ],
          "gas_limit": 4109798337
        }"#;

        authenticate_tx(ctx.as_auth(), tx.deserialize_json::<Tx>().unwrap(), None)
            .should_fail_with_error("recovered Ethereum address does not match");
    }

    /// Regression test for security audit Finding 16:
    /// `max + MAX_NONCE_INCREASE` overflows when `max > u32::MAX - 100`.
    ///
    /// In debug builds (wrapping arithmetic), the addition wraps to a small
    /// number, causing valid nonces near `u32::MAX` to be incorrectly rejected
    /// with "nonce is too far ahead". In release builds (`overflow-checks = true`),
    /// the addition panics, permanently locking the account.
    #[test]
    fn nonce_near_u32_max_does_not_overflow() {
        let user_address = addr!("843a9778a711d5474ef6efd65fca38731f281471");
        let user_index = 231893934;
        let user_keyhash =
            hash!("94eb754d36ed86af6fc231eae13c78b4a298ed065f63eb8dac139b8b943b76da");
        let user_key = Key::Secp256k1(
            hex!("022e730b2e26c6e3ce78d28c3700da0a798d893a73ee15055baaaee1cf46db7a4a").into(),
        );

        // Pre-seed SEEN_NONCES with a value near u32::MAX.
        // u32::MAX - 50 = 4294967245
        let mut storage = MockStorage::new();
        account::SEEN_NONCES
            .save(&mut storage, &BTreeSet::from([u32::MAX - 50]))
            .unwrap();

        let querier = MockQuerier::new()
            .with_app_config(AppConfig {
                addresses: AppAddresses {
                    account_factory: ACCOUNT_FACTORY,
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap()
            .with_raw_contract_storage(ACCOUNT_FACTORY, |storage| {
                let user = User {
                    index: user_index,
                    name: Username::default_for_index(user_index),
                    accounts: btree_map! { 0u32 => user_address },
                    keys: btree_map! { user_keyhash => user_key },
                };
                USERS.save(storage, user_index, &user).unwrap();
            });

        // Use nonce = u32::MAX - 49 which is within MAX_NONCE_INCREASE (100)
        // of the max seen nonce (u32::MAX - 50).
        //
        // Before fix: (u32::MAX - 50) + 100 wraps to 49 in debug,
        //   so u32::MAX - 49 <= 49 is false → "nonce is too far ahead".
        // After fix: (u32::MAX - 50).saturating_add(100) = u32::MAX,
        //   so u32::MAX - 49 <= u32::MAX is true → passes nonce check.
        let nonce: u32 = u32::MAX - 49;
        let tx = format!(
            r#"{{
              "sender": "0x843a9778a711d5474ef6efd65fca38731f281471",
              "gas_limit": 1000000,
              "msgs": [
                {{
                  "transfer": {{
                    "0x836ca678a5afe736c6b64b2d5a6ee4bc85588cd8": {{
                      "bridge/usdc": "100000000"
                    }}
                  }}
                }}
              ],
              "data": {{
                "chain_id": "dev-1",
                "nonce": {nonce},
                "user_index": 231893934
              }},
              "credential": {{
                "standard": {{
                  "key_hash": "94EB754D36ED86AF6FC231EAE13C78B4A298ED065F63EB8DAC139B8B943B76DA",
                  "signature": {{
                    "secp256k1": "LEohIzCuV3/MRLM/XvZNcxUdNp/Q811IsioZ3SEBbbRMvXlrvWi1v3+NYeZBYnALVtQzZcpO1E2wqiBd64lSdg=="
                  }}
                }}
              }}
            }}"#
        );

        let user = User {
            index: user_index,
            name: Username::default_for_index(user_index),
            accounts: btree_map! { 0u32 => user_address },
            keys: btree_map! { user_keyhash => user_key },
        };

        let mut ctx = MockContext::new()
            .with_storage(storage)
            .with_querier(querier)
            .with_contract(user_address)
            .with_chain_id("dev-1")
            .with_mode(AuthMode::Finalize);

        let result =
            verify_nonce_and_signature(ctx.as_auth(), tx.deserialize_json().unwrap(), &user, None);

        // The nonce check must NOT cause an overflow or reject the nonce.
        // It may fail later (e.g., signature mismatch), but not here.
        let err_str = result.unwrap_err().to_string();
        assert!(
            !err_str.contains("nonce is too far ahead"),
            "nonce near u32::MAX caused overflow rejection: {err_str}"
        );
    }
}
