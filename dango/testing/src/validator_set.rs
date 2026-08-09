use {
    crate::constants::{MOCK_HYPERLANE_REMOTE_MERKLE_TREE, MOCK_HYPERLANE_VALIDATOR_SIGNING_KEYS},
    dango_hyperlane_types::{
        Addr32, IncrementalMerkleTree, domain_hash, eip191_hash,
        isms::{HYPERLANE_DOMAIN_KEY, multisig::Metadata},
        mailbox::{self, Domain, MAILBOX_VERSION},
        multisig_hash,
    },
    dango_primitives::{Addr, Hash256, HashExt, HexBinary, HexByteArray, Inner, Shared},
    k256::ecdsa::SigningKey,
    std::collections::{BTreeSet, HashMap},
};

pub struct MockValidatorSets(HashMap<Domain, MockValidatorSet>);

impl MockValidatorSets {
    pub fn new_preset(domains: &[Domain], random_nonce: bool) -> Self {
        Self(
            domains
                .iter()
                .map(|&d| (d, MockValidatorSet::new_preset(d, random_nonce)))
                .collect(),
        )
    }

    pub fn get(&self, domain: Domain) -> Option<&MockValidatorSet> {
        self.0.get(&domain)
    }

    pub fn insert(&mut self, domain: Domain, validator_set: MockValidatorSet) {
        self.0.insert(domain, validator_set);
    }
}

pub enum Nonce {
    Random,
    Sequential(u32),
}

impl Nonce {
    pub fn new(random_nonce: bool) -> Self {
        if random_nonce {
            Self::Random
        } else {
            Self::Sequential(0)
        }
    }

    /// Get the next nonce.
    pub fn next_nonce(&mut self) -> u32 {
        match self {
            Self::Random => rand::random(),
            Self::Sequential(nonce) => {
                *nonce += 1;
                *nonce
            },
        }
    }
}

/// A mock up Hyperlane validator set for testing purpose.
#[derive(Clone)]
pub struct MockValidatorSet {
    domain: Domain,
    validators: Vec<k256::ecdsa::SigningKey>,
    merkle_tree_address: Addr32,
    merkle_tree_and_nonce: Shared<(IncrementalMerkleTree, Nonce)>,
}

impl MockValidatorSet {
    /// Create new a mock validator set of a domain with the given validators
    /// signing keys.
    pub fn new<I>(
        domain: Domain,
        merkle_tree_address: Addr32,
        signing_keys: I,
        random_nonce: bool,
    ) -> Self
    where
        I: IntoIterator<Item = dango_eth_utils::SigningKey>,
    {
        // Parse the raw signing key bytes, and derive the corresponding
        // Ethereum addresses.
        let validators = signing_keys
            .into_iter()
            .map(|sk_raw| SigningKey::from_bytes(&sk_raw.into()).unwrap())
            .collect();

        Self {
            domain,
            validators,
            merkle_tree_address,
            merkle_tree_and_nonce: Shared::new((
                IncrementalMerkleTree::default(),
                Nonce::new(random_nonce),
            )),
        }
    }

    /// Create a new mock validator set of a domain with a preset validator set.
    pub fn new_preset(domain: Domain, random_nonce: bool) -> Self {
        Self::new(
            domain,
            MOCK_HYPERLANE_REMOTE_MERKLE_TREE,
            MOCK_HYPERLANE_VALIDATOR_SIGNING_KEYS,
            random_nonce,
        )
    }

    /// Pretend a given message have been dispatched at the Mailbox contract on
    /// the domain. Sign signatures to testify for this message.
    ///
    /// Return the message ID, raw message, and raw metadata that can be
    /// submitted to the Mailbox contract on Dango for processing.
    pub fn sign(
        &self,
        sender: Addr32,
        destination_domain: Domain, // This should be the domain of Dango.
        recipient: Addr,
        body: HexBinary,
    ) -> (Hash256, HexBinary, HexBinary) {
        // Insert the message into the Merkle tree.
        let (raw_message, message_id, merkle_root, merkle_index) =
            self.new_message(sender, destination_domain, recipient, body);

        // Compute the hash that needs to be signed by the validators.
        let multisig_hash = eip191_hash(multisig_hash(
            domain_hash(self.domain, self.merkle_tree_address, HYPERLANE_DOMAIN_KEY),
            merkle_root,
            merkle_index,
            message_id,
        ));

        // Each validator signs the hash.
        let signatures = self
            .validators
            .iter()
            .map(|sk| {
                let signature = dango_eth_utils::sign_digest(multisig_hash.into_inner(), sk);
                HexByteArray::from_inner(signature)
            })
            .collect();

        // Compose the metadata and encode it to raw bytes.
        let raw_metadata = Metadata {
            origin_merkle_tree: self.merkle_tree_address,
            merkle_root,
            merkle_index,
            signatures,
        }
        .encode();

        (message_id, raw_message, raw_metadata)
    }

    pub fn validator_addresses(&self) -> BTreeSet<HexByteArray<20>> {
        self.validators
            .iter()
            .map(|sk| HexByteArray::from_inner(dango_eth_utils::derive_address(sk.verifying_key())))
            .collect()
    }

    /// Increment the nonce and insert the message into the merkle tree.
    ///
    /// Returns:
    ///
    /// - The raw message
    /// - The message ID
    /// - Merkle root
    /// - Merkle index
    fn new_message(
        &self,
        sender: Addr32,
        destination_domain: Domain, // This should be the domain of Dango.
        recipient: Addr,
        body: HexBinary,
    ) -> (HexBinary, Hash256, Hash256, u32) {
        self.merkle_tree_and_nonce.write_with(|mut guard| {
            // Destructure the write guard. This isn't necessary but makes our
            // syntax look cleaner.
            let (merkle_tree, nonce) = &mut *guard;

            // Get the next nonce.
            let next_nonce = nonce.next_nonce();

            // Compose the Hyperlane message and encode it to raw bytes.
            let raw_message = mailbox::Message {
                version: MAILBOX_VERSION,
                nonce: next_nonce,
                origin_domain: self.domain,
                sender,
                destination_domain,
                recipient: recipient.into(),
                body,
            }
            .encode();

            // Hash the raw message.
            let message_id = raw_message.keccak256();

            // Get the Merkle index. Note we need the index _before_ the insertion.
            let merkle_index = merkle_tree.count as u32;

            // Insert the message into the merkle tree.
            merkle_tree.insert(message_id).unwrap();

            // Return the raw message, merkle root and index.
            (raw_message, message_id, merkle_tree.root(), merkle_index)
        })
    }
}
