use {
    crate::{BitArray, hash_internal_node, hash_leaf_node},
    dango_primitives::{Hash256, MembershipProof, NonMembershipProof, Order, Proof, ProofNode},
    thiserror::Error,
};

#[derive(Debug, Error)]
pub enum ProofError {
    #[error("incorrect proof type, expect {expect}, got {actual}")]
    IncorrectProofType {
        expect: &'static str,
        actual: &'static str,
    },

    #[error("root hash mismatch! computed: {computed}, actual: {actual}")]
    RootHashMismatch { computed: Hash256, actual: Hash256 },

    // TODO: add more details to the error message?
    #[error("expecting child to not exist but it exists")]
    UnexpectedChild,

    // TODO: add more details to the error message?
    #[error("expecting bitarrays to share a common prefix but they do not")]
    NotCommonPrefix,
}

pub fn verify_proof(
    root_hash: Hash256,
    key_hash: Hash256,
    value_hash: Option<Hash256>,
    proof: &Proof,
) -> Result<(), ProofError> {
    match (value_hash, proof) {
        (Some(value_hash), Proof::Membership(proof)) => {
            verify_membership_proof(root_hash, key_hash, value_hash, proof)
        },
        (None, Proof::NonMembership(proof)) => {
            verify_non_membership_proof(root_hash, key_hash, proof)
        },
        (Some(_), Proof::NonMembership(_)) => Err(ProofError::IncorrectProofType {
            expect: "membership",
            actual: "non-membership",
        }),
        (None, Proof::Membership(_)) => Err(ProofError::IncorrectProofType {
            expect: "non-membership",
            actual: "membership",
        }),
    }
}

pub fn verify_membership_proof(
    root_hash: Hash256,
    key_hash: Hash256,
    value_hash: Hash256,
    proof: &MembershipProof,
) -> Result<(), ProofError> {
    let bitarray = BitArray::from_bytes(&key_hash);
    let hash = hash_leaf_node(key_hash, value_hash);

    compute_and_compare_root_hash(root_hash, bitarray, &proof.sibling_hashes, hash)
}

pub fn verify_non_membership_proof(
    root_hash: Hash256,
    key_hash: Hash256,
    proof: &NonMembershipProof,
) -> Result<(), ProofError> {
    let bitarray = BitArray::from_bytes(&key_hash);
    let hash = match proof.node {
        // If the node given is an internal node, we check the bit at the depth.
        // If the bit is a 0, it must not have a left child; if the bit is a 1,
        // it must not have a right child.
        ProofNode::Internal {
            left_hash,
            right_hash,
        } => {
            match (
                bitarray.bit_at_index(proof.sibling_hashes.len()),
                left_hash,
                right_hash,
            ) {
                (0, Some(_), _) | (1, _, Some(_)) => {
                    return Err(ProofError::UnexpectedChild);
                },
                _ => hash_internal_node(left_hash, right_hash),
            }
        },
        // If the node given is a leaf, it's bit path must share a common prefix
        // with the key we want to prove not exist.
        ProofNode::Leaf {
            key_hash,
            value_hash,
        } => {
            let non_exist_bitarray = BitArray::from_bytes(&key_hash);
            let exist_bits =
                bitarray.range(None, Some(proof.sibling_hashes.len()), Order::Descending);
            let non_exist_bits =
                non_exist_bitarray.range(None, Some(proof.sibling_hashes.len()), Order::Descending);
            if exist_bits.zip(non_exist_bits).any(|(a, b)| a != b) {
                return Err(ProofError::NotCommonPrefix);
            }
            hash_leaf_node(key_hash, value_hash)
        },
    };

    compute_and_compare_root_hash(root_hash, bitarray, &proof.sibling_hashes, hash)
}

fn compute_and_compare_root_hash(
    root_hash: Hash256,
    bitarray: BitArray,
    sibling_hashes: &[Option<Hash256>],
    mut hash: Hash256,
) -> Result<(), ProofError> {
    for (bit, sibling_hash) in bitarray
        .range(None, Some(sibling_hashes.len()), Order::Descending)
        .zip(sibling_hashes)
    {
        if bit == 0 {
            hash = hash_internal_node(Some(hash), *sibling_hash);
        } else {
            hash = hash_internal_node(*sibling_hash, Some(hash));
        }
    }

    if hash != root_hash {
        return Err(ProofError::RootHashMismatch {
            computed: hash,
            actual: root_hash,
        });
    }

    Ok(())
}

// ----------------------------------- tests -----------------------------------

#[cfg(test)]
mod tests {
    use {
        super::*,
        dango_primitives::{Hash256, HashExt, NonMembershipProof},
        hex_literal::hex,
        test_case::test_case,
    };

    // use the same test case as in tree.rs
    const HASH_ROOT: Hash256 = Hash256::from_inner(hex!(
        "ae08c246d53a8ff3572a68d5bba4d610aaaa765e3ef535320c5653969aaa031b"
    ));
    const HASH_0: Hash256 = Hash256::from_inner(hex!(
        "b843a96765fc40641227234e9f9a2736c2e0cdf8fb2dc54e358bb4fa29a61042"
    ));
    const HASH_1: Hash256 = Hash256::from_inner(hex!(
        "cb640e68682628445a3e0713fafe91b9cefe4f81c2337e9d3df201d81ae70222"
    ));
    const HASH_01: Hash256 = Hash256::from_inner(hex!(
        "521de0a3ef2b7791666435a872ca9ec402ce886aff07bb4401de28bfdde4a13b"
    ));
    const HASH_010: Hash256 = Hash256::from_inner(hex!(
        "c8348e9a7a327e8b76e97096c362a1f87071ee4108b565d1f409529c189cb684"
    ));
    const HASH_011: Hash256 = Hash256::from_inner(hex!(
        "e104e2bcf24027af737c021033cb9d8cbd710a463f54ae6f2ff9eb06c784c744"
    ));
    const HASH_0110: Hash256 = Hash256::from_inner(hex!(
        "fd34e3f8d9840e7f6d6f639435b6f9b67732fc5e3d5288e268021aeab873f280"
    ));
    const HASH_0111: Hash256 = Hash256::from_inner(hex!(
        "412341380b1e171077dd9da9af936ae2126ede2dd91dc5acb0f77363d46eb76b"
    ));
    const HASH_M: Hash256 = Hash256::from_inner(hex!(
        "62c66a7a5dd70c3146618063c344e531e6d4b59e379808443ce962b3abd63c5a"
    ));
    const HASH_BAR: Hash256 = Hash256::from_inner(hex!(
        "fcde2b2edba56bf408601fb721fe9b5c338d10ee429ea04fae5511b68fbf8fb9"
    ));

    #[test_case(
        "r",
        "foo",
        MembershipProof {
            sibling_hashes: vec![
                Some(HASH_011),
                None,
                Some(HASH_1),
            ],
        };
        "proving (r, foo)"
    )]
    #[test_case(
        "m",
        "bar",
        MembershipProof {
            sibling_hashes: vec![
                Some(HASH_0111),
                Some(HASH_010),
                None,
                Some(HASH_1),
            ],
        };
        "proving (m, bar)"
    )]
    #[test_case(
        "L",
        "fuzz",
        MembershipProof {
            sibling_hashes: vec![
                Some(HASH_0110),
                Some(HASH_010),
                None,
                Some(HASH_1),
            ],
        };
        "proving (L, fuzz)"
    )]
    #[test_case(
        "a",
        "buzz",
        MembershipProof {
            sibling_hashes: vec![Some(HASH_0)],
        };
        "proving (a, buzz)"
    )]
    fn verifying_membership(key: &str, value: &str, proof: MembershipProof) {
        assert!(
            verify_membership_proof(
                HASH_ROOT,
                key.as_bytes().sha2_256(),
                value.as_bytes().sha2_256(),
                &proof,
            )
            .is_ok()
        );
    }

    #[test_case(
        "b",
        NonMembershipProof {
            node: ProofNode::Internal {
                left_hash:  None,
                right_hash: Some(HASH_01),
            },
            sibling_hashes: vec![Some(HASH_1)],
        };
        "proving b"
    )]
    #[test_case(
        "o",
        NonMembershipProof {
            node: ProofNode::Leaf {
                key_hash:   HASH_M,
                value_hash: HASH_BAR,
            },
            sibling_hashes: vec![
                Some(HASH_0111),
                Some(HASH_010),
                None,
                Some(HASH_1),
            ],
        };
        "proving o"
    )]
    fn verifying_non_membership(key: &str, proof: NonMembershipProof) {
        assert!(verify_non_membership_proof(HASH_ROOT, key.as_bytes().sha2_256(), &proof).is_ok());
    }

    // TODO: add fail cases for proofs
}
