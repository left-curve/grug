use {
    crate::VALIDATOR_SETS,
    anyhow::ensure,
    dango_hyperlane_types::{
        domain_hash, eip191_hash, is_canonical_ecdsa_signature,
        isms::{
            HYPERLANE_DOMAIN_KEY, IsmQuery, IsmQueryResponse,
            multisig::{Metadata, QueryMsg, ValidatorSet},
        },
        mailbox::{Domain, Message},
        multisig_hash,
    },
    dango_primitives::{
        Bound, DEFAULT_PAGE_LIMIT, HashExt, HexByteArray, ImmutableCtx, Inner, Json, JsonSerExt,
        Order, StdResult,
    },
    std::collections::{BTreeMap, BTreeSet},
};

pub fn query(ctx: ImmutableCtx, msg: QueryMsg) -> anyhow::Result<Json> {
    match msg {
        QueryMsg::ValidatorSet { domain } => {
            let res = query_validator_set(ctx, domain)?;
            res.to_json_value()
        },
        QueryMsg::ValidatorSets { start_after, limit } => {
            let res = query_validator_sets(ctx, start_after, limit)?;
            res.to_json_value()
        },
        QueryMsg::Ism(IsmQuery::Verify {
            raw_message,
            raw_metadata,
        }) => {
            let res = IsmQueryResponse::Verify(verify(ctx, &raw_message, &raw_metadata)?);
            res.to_json_value()
        },
    }
    .map_err(Into::into)
}

#[inline]
fn query_validator_set(ctx: ImmutableCtx, domain: Domain) -> StdResult<ValidatorSet> {
    VALIDATOR_SETS.load(ctx.storage, domain)
}

#[inline]
fn query_validator_sets(
    ctx: ImmutableCtx,
    start_after: Option<Domain>,
    limit: Option<u32>,
) -> StdResult<BTreeMap<Domain, ValidatorSet>> {
    let start = start_after.map(Bound::Exclusive);
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT);

    VALIDATOR_SETS
        .range(ctx.storage, start, None, Order::Ascending)
        .take(limit as usize)
        .collect()
}

fn verify(ctx: ImmutableCtx, raw_message: &[u8], raw_metadata: &[u8]) -> anyhow::Result<()> {
    let message = Message::decode(raw_message)?;
    let metadata = Metadata::decode(raw_metadata)?;

    // Ensure the message contains the appropriate number of signatures.
    //
    // On the one hand, the number must be no less than the threshold, for the
    // message to be considered valid.
    //
    // On the other hand, the number can't be too big, otherwise an attackr can
    // DoS the chain by fabricating a message with a great amount of signatures.
    // The contract would need to verify them all, consuming onchain computing
    // resources. Here, we use the size of the validator set as a reasonble
    // upper bound.
    let validator_set = VALIDATOR_SETS.load(ctx.storage, message.origin_domain)?;
    let min = validator_set.threshold as usize;
    let max = validator_set.validators.len();

    ensure!(
        (min..=max).contains(&metadata.signatures.len()),
        "invalid number of signatures! expecting between {} and {}, got {}",
        min,
        max,
        metadata.signatures.len()
    );

    // This is the hash that validators are supposed to sign.
    let multisig_hash = eip191_hash(multisig_hash(
        domain_hash(
            message.origin_domain,
            metadata.origin_merkle_tree,
            HYPERLANE_DOMAIN_KEY,
        ),
        metadata.merkle_root,
        metadata.merkle_index,
        raw_message.keccak256(),
    ));

    // Loop through the signatures and recover the addresses.
    let signers = metadata
        .signatures
        .into_iter()
        .map(|signature| {
            let v = signature[64];
            ensure!(v == 27 || v == 28, "invalid recovery id: {v}");
            ensure!(
                is_canonical_ecdsa_signature(signature.inner()),
                "non-canonical (high-s) signature"
            );

            let pk = ctx.api.secp256k1_pubkey_recover(
                &multisig_hash,
                &signature[..64],
                v - 27, // Ethereum uses recovery IDs 27, 28 instead of 0, 1.
                false,  // We need the _uncompressed_ public key for deriving address!
            )?;
            let pk_hash = ctx.api.keccak256(&pk[1..]);
            let address = &pk_hash[12..];

            Ok(HexByteArray::from_inner(address.try_into().unwrap()))
        })
        .collect::<anyhow::Result<BTreeSet<_>>>()?;

    // Ensure there are enough unique signers to meet the threshold.
    // This prevents signature malleability attacks where an attacker submits
    // multiple distinct signatures (e.g. (r, s, v) and (r, n-s, 1-v)) that
    // recover to the same address, passing the raw count check above while
    // only controlling fewer keys than the threshold requires.
    ensure!(
        signers.len() >= min,
        "not enough unique signers! expecting at least {}, got {}",
        min,
        signers.len()
    );

    // Ensure all signatures are from legit validators.
    ensure!(
        signers.is_subset(&validator_set.validators),
        "recovered addresses is not a strict subset of the validator set"
    );

    Ok(())
}

// ----------------------------------- tests -----------------------------------

// Adapted from:
// https://github.com/many-things/cw-hyperlane/blob/d07e55e17c791a5f6557f114e3fb6cb433d9b800/contracts/isms/multisig/src/query.rs#L111-L190
#[cfg(test)]
mod tests {
    use {
        super::*,
        dango_hyperlane_types::{Addr32, IncrementalMerkleTree, addr32, mailbox::MAILBOX_VERSION},
        dango_primitives::{Inner, MockContext, ResultExt, btree_set},
        hex_literal::hex,
        k256::elliptic_curve::Generate,
        test_case::test_case,
    };

    /// Mock address.
    const ZERO_ADDRESS: Addr32 =
        addr32!("0000000000000000000000000000000000000000000000000000000000000000");

    #[test_case(
        hex!("0000000000000068220000000000000000000000000d1255b09d94659bb0888e0aa9fca60245ce402a0000682155208cd518cffaac1b5d8df216a9bd050c9a03f0d4f3ba88e5268ac4cd12ee2d68656c6c6f"),
        hex!("986a1625d44e4b3969b08a5876171b2b4fcdf61b3e5c70a86ad17b304f17740a9f45d99ea6bec61392a47684f4e5d1416ddbcb5fdef0f132c27d7034e9bbff1c00000000ba9911d78ec6d561413e3589f920388cbd7554fbddd8ce50739337250853ec3577a51fa40e727c05b50f15db13f5aad5857c89d432644be48d70325ea83fdb6c1c"),
        btree_set! {
            HexByteArray::from_inner(hex!("122e0663ccc190266427e7fc0ed6589b5d7d36db")),
            HexByteArray::from_inner(hex!("01d7525e91dfc3f594fd366aad70f956b398de9e")),
        };
        "one signature"
    )]
    #[case(
        hex!("03000000240001388100000000000000000000000004980c17e2ce26578c82f81207e706e4505fae3b0000a8690000000000000000000000000b1c1b54f45e02552331d3106e71f5e0b573d5d448656c6c6f21"),
        hex!("0000000000000000000000009af85731edd41e2e50f81ef8a0a69d2fb836edf9a84430f822e0e9b5942faace72bd5b97f0b59a58a9b8281231d9e5c393b5859c00000024539feceace17782697e29e74151006dc7b47227cf48aba02926336cb5f7fa38b3d05e8293045f7b5811eda3ae8aa070116bb5fbf57c79e143a69e909df90cefa1b6e6ead7180e0415c36642ee4bc5454bc4f5ca250ca77a1a83562035544e0e898734d6541a20404e05fd53eb1c75b0bd21851c3bd8122cfa3550d7b6fb94d7cee1b"),
        btree_set!{
            HexByteArray::from_inner(hex!("ebc301013b6cd2548e347c28d2dc43ec20c068f2")),
            HexByteArray::from_inner(hex!("315db9868fc8813b221b1694f8760ece39f45447")),
            HexByteArray::from_inner(hex!("17517c98358c5937c5d9ee47ce1f5b4c2b7fc9f5")),
        };
        "two signatures"
    )]
    fn verify_with_e2e_data<const A: usize, const B: usize>(
        raw_message: [u8; A],
        raw_metadata: [u8; B],
        validators: BTreeSet<HexByteArray<20>>,
    ) {
        let mut ctx = MockContext::new();
        let mut message = Message::decode(&raw_message).unwrap();

        VALIDATOR_SETS
            .save(
                &mut ctx.storage,
                message.origin_domain,
                &ValidatorSet {
                    threshold: 1,
                    validators,
                },
            )
            .unwrap();

        verify(ctx.as_immutable(), &raw_message, &raw_metadata).should_succeed();

        // Try forging a false message. Verification should fail.
        message.body = b"larry".to_vec().into();

        verify(ctx.as_immutable(), &message.encode(), &raw_metadata).should_fail();
    }

    /// Test three scenarios:
    ///
    /// 1. Number of signatures is less than threshold.
    /// 2. Number of signatures is greater than the size of validator set.
    /// 3. A signature is from an unknown signer (who is not in the validator set).
    #[test]
    fn rejecting_too_many_or_too_few_or_unknown_signatures() {
        let mut ctx = MockContext::new();

        // ------------------------ 1. Prepare message -------------------------

        // Create a mock message.
        let message = Message {
            version: MAILBOX_VERSION,
            nonce: 0,
            origin_domain: 0,
            sender: ZERO_ADDRESS,
            destination_domain: 0,
            recipient: ZERO_ADDRESS,
            body: Vec::new().into(),
        };

        let raw_message = message.encode();
        let message_id = raw_message.keccak256();

        // Insert the message into Merkle tree.
        let mut merkle_tree = IncrementalMerkleTree::default();
        merkle_tree.insert(message_id).unwrap();

        let merkle_root = merkle_tree.root();
        let merkle_index = (merkle_tree.count - 1) as u32;

        // Create the hash that the validators need to sign.
        let multisig_hash = eip191_hash(multisig_hash(
            domain_hash(message.origin_domain, ZERO_ADDRESS, HYPERLANE_DOMAIN_KEY),
            merkle_root,
            merkle_index,
            message_id,
        ));

        // --------------------- 2. Prepare validator set ----------------------

        // Generate 4 validator keys.
        let validators = (0..4)
            .map(|_| k256::ecdsa::SigningKey::generate())
            .collect::<Vec<_>>();

        // Derive the corresponding Ethereum addresses.
        let validator_set = validators
            .iter()
            .map(|sk| {
                let pk = k256::ecdsa::VerifyingKey::from(sk)
                    .to_sec1_point(false)
                    .to_bytes();
                let pk_hash = (&pk[1..]).keccak256();
                HexByteArray::from_inner(pk_hash[12..].try_into().unwrap())
            })
            .collect::<Vec<_>>();

        // The 4 validators sign the message.
        let signatures = validators
            .iter()
            .map(|sk| {
                let (signature, recovery_id) =
                    sk.sign_prehash_recoverable(&multisig_hash.into_inner());
                let mut packed = [0_u8; 65];
                packed[..64].copy_from_slice(&signature.to_bytes());
                packed[64] = recovery_id.to_byte() + 27;
                HexByteArray::from_inner(packed)
            })
            .collect::<Vec<_>>();

        // Save the _only the first three_ validators with a threshold of 2.
        VALIDATOR_SETS
            .save(
                &mut ctx.storage,
                message.origin_domain,
                &ValidatorSet {
                    threshold: 2,
                    validators: validator_set[..3].iter().copied().collect(),
                },
            )
            .unwrap();

        // ----------------------- 3. Verify signatures ------------------------

        // 2 or 3 signatures. Should succeed.
        for num in [2, 3] {
            verify(
                ctx.as_immutable(),
                &raw_message,
                &Metadata {
                    origin_merkle_tree: ZERO_ADDRESS,
                    merkle_root,
                    merkle_index,
                    signatures: signatures[..num].iter().cloned().collect(),
                }
                .encode(),
            )
            .should_succeed();
        }

        // 1 or 4 signatures. Should fail.
        for num in [1, 4] {
            verify(
                ctx.as_immutable(),
                &raw_message,
                &Metadata {
                    origin_merkle_tree: ZERO_ADDRESS,
                    merkle_root,
                    merkle_index,
                    signatures: signatures[..num].iter().cloned().collect(),
                }
                .encode(),
            )
            .should_fail_with_error(format!(
                "invalid number of signatures! expecting between 2 and 3, got {num}"
            ));
        }

        // 3 signatures, but one of which is from an unknown signer.
        verify(
            ctx.as_immutable(),
            &raw_message,
            &Metadata {
                origin_merkle_tree: ZERO_ADDRESS,
                merkle_root,
                merkle_index,
                signatures: btree_set! {
                    signatures[0],
                    signatures[1],
                    signatures[3], // unknown signer
                },
            }
            .encode(),
        )
        .should_fail_with_error("recovered addresses is not a strict subset of the validator set");

        // 2 signatures, but they are the same. This is essentially just one signature.
        verify(
            ctx.as_immutable(),
            &raw_message,
            &Metadata {
                origin_merkle_tree: ZERO_ADDRESS,
                merkle_root,
                merkle_index,
                signatures: btree_set! {
                    signatures[0],
                    signatures[0], // same signature
                },
            }
            .encode(),
        )
        .should_fail_with_error("invalid number of signatures! expecting between 2 and 3, got 1");
    }

    /// Test that signature malleability (Finding 1) is rejected.
    ///
    /// For any ECDSA signature (r, s, v), the variant (r, n-s, v^1) is
    /// byte-distinct but recovers to the same signer. An attacker who
    /// controls only one validator key could submit both variants to meet
    /// a threshold > 1. Two layers of defense catch this:
    /// 1. `is_canonical_ecdsa_signature` rejects high-s signatures outright.
    /// 2. `signers.len() >= threshold` as a backstop after recovery.
    #[test]
    fn rejecting_malleable_duplicate_signatures() {
        let mut ctx = MockContext::new();

        // ------------------------ 1. Prepare message -------------------------

        let message = Message {
            version: MAILBOX_VERSION,
            nonce: 0,
            origin_domain: 0,
            sender: ZERO_ADDRESS,
            destination_domain: 0,
            recipient: ZERO_ADDRESS,
            body: Vec::new().into(),
        };

        let raw_message = message.encode();
        let message_id = raw_message.keccak256();

        let mut merkle_tree = IncrementalMerkleTree::default();
        merkle_tree.insert(message_id).unwrap();

        let merkle_root = merkle_tree.root();
        let merkle_index = (merkle_tree.count - 1) as u32;

        let multisig_hash = eip191_hash(multisig_hash(
            domain_hash(message.origin_domain, ZERO_ADDRESS, HYPERLANE_DOMAIN_KEY),
            merkle_root,
            merkle_index,
            message_id,
        ));

        // --------------------- 2. Prepare validator set ----------------------

        let validators = (0..3)
            .map(|_| k256::ecdsa::SigningKey::generate())
            .collect::<Vec<_>>();

        let validator_set = validators
            .iter()
            .map(|sk| {
                let pk = k256::ecdsa::VerifyingKey::from(sk)
                    .to_sec1_point(false)
                    .to_bytes();
                let pk_hash = (&pk[1..]).keccak256();
                HexByteArray::from_inner(pk_hash[12..].try_into().unwrap())
            })
            .collect::<Vec<_>>();

        // Validator 0 signs the message.
        let (signature, recovery_id) =
            validators[0].sign_prehash_recoverable(&multisig_hash.into_inner());

        let v = recovery_id.to_byte() + 27;
        let mut original = [0u8; 65];
        original[..64].copy_from_slice(&signature.to_bytes());
        original[64] = v;

        // Construct the malleable variant: (r, n-s, flipped_v).
        // Negating the scalar computes n - s (the high-s counterpart).
        let neg_s = (-*signature.s()).to_bytes();
        let mut malleable = [0u8; 65];
        malleable[..32].copy_from_slice(&original[..32]); // same r
        malleable[32..64].copy_from_slice(&neg_s); // n - s
        malleable[64] = if v == 27 {
            28
        } else {
            27
        }; // flipped recovery id

        // The two byte arrays must be distinct (otherwise BTreeSet dedupes them
        // and the test wouldn't exercise the malleability path).
        assert_ne!(original, malleable);

        // Save with threshold = 2 so a single signer shouldn't suffice.
        VALIDATOR_SETS
            .save(
                &mut ctx.storage,
                message.origin_domain,
                &ValidatorSet {
                    threshold: 2,
                    validators: validator_set.iter().copied().collect(),
                },
            )
            .unwrap();

        // -------------------- 3. Malleability must fail ----------------------

        // The high-s variant is rejected at the boundary before recovery.
        verify(
            ctx.as_immutable(),
            &raw_message,
            &Metadata {
                origin_merkle_tree: ZERO_ADDRESS,
                merkle_root,
                merkle_index,
                signatures: btree_set! {
                    HexByteArray::from_inner(original),
                    HexByteArray::from_inner(malleable),
                },
            }
            .encode(),
        )
        .should_fail_with_error("non-canonical (high-s) signature");

        // ------------------- 4. Sanity: two real signers ---------------------

        // Two distinct validators should succeed (proves the test setup is valid).
        let (sig1, rid1) = validators[1].sign_prehash_recoverable(&multisig_hash.into_inner());
        let mut packed1 = [0u8; 65];
        packed1[..64].copy_from_slice(&sig1.to_bytes());
        packed1[64] = rid1.to_byte() + 27;

        verify(
            ctx.as_immutable(),
            &raw_message,
            &Metadata {
                origin_merkle_tree: ZERO_ADDRESS,
                merkle_root,
                merkle_index,
                signatures: btree_set! {
                    HexByteArray::from_inner(original),
                    HexByteArray::from_inner(packed1),
                },
            }
            .encode(),
        )
        .should_succeed();
    }
}
