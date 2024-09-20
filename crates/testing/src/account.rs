use {
    grug_crypto::{sha2_256, Identity256},
    grug_mock_account::{Credential, PublicKey},
    grug_types::{
        Addr, Addressable, Hash256, Json, JsonSerExt, Message, Signer, StdResult, Tx,
        GENESIS_SENDER,
    },
    k256::ecdsa::{signature::DigestSigner, Signature, SigningKey},
    rand::rngs::OsRng,
    std::collections::HashMap,
};

/// A signer that tracks a sequence number and signs transactions in a way
/// corresponding to the mock account used in Grug test suite.
pub struct TestAccount {
    pub address: Addr,
    pub sk: SigningKey,
    pub pk: PublicKey,
    pub sequence: u32,
}

impl TestAccount {
    /// Create a new test account with a random Secp256k1 key pair.
    ///
    /// The address is predicted with the given code hash and salt, assuming the
    /// account is to be instantiated during genesis.
    pub fn new_random(code_hash: Hash256, salt: &[u8]) -> Self {
        let address = Addr::compute(GENESIS_SENDER, code_hash, salt);
        let sk = SigningKey::random(&mut OsRng);
        let pk = sk
            .verifying_key()
            .to_encoded_point(true)
            .to_bytes()
            .to_vec()
            .try_into()
            .expect("pk is of wrong length");

        Self {
            address,
            sk,
            pk,
            sequence: 0,
        }
    }

    /// Sign a transaction with the given sequence, without considering or
    /// updating the internally tracked sequence.
    pub fn sign_transaction_with_sequence(
        &self,
        msgs: Vec<Message>,
        chain_id: &str,
        sequence: u32,
        gas_limit: u64,
    ) -> StdResult<Tx> {
        let sign_bytes = Identity256::from(grug_mock_account::make_sign_bytes(
            sha2_256,
            &msgs,
            self.address,
            chain_id,
            sequence,
        )?);

        let signature: Signature = self.sk.sign_digest(sign_bytes);

        let credential = Credential {
            signature: signature.to_vec().try_into()?,
            sequence,
        }
        .to_json_value()?;

        Ok(Tx {
            sender: self.address,
            gas_limit,
            msgs,
            data: Json::Null,
            credential,
        })
    }
}

impl Addressable for TestAccount {
    fn address(&self) -> Addr {
        self.address
    }
}

impl Signer for TestAccount {
    fn sign_transaction(
        &mut self,
        msgs: Vec<Message>,
        chain_id: &str,
        gas_limit: u64,
    ) -> StdResult<Tx> {
        let sequence = self.sequence;
        self.sequence += 1;
        self.sign_transaction_with_sequence(msgs, chain_id, sequence, gas_limit)
    }
}

pub type TestAccounts = HashMap<&'static str, TestAccount>;
