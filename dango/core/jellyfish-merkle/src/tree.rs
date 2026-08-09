use {
    crate::{BitArray, Child, InternalNode, LeafNode, Node},
    dango_app::Commitment,
    dango_primitives::{
        Batch, Hash256, HashExt, MembershipProof, NonMembershipProof, Op, Order, Proof, ProofNode,
        StdResult, Storage,
    },
    dango_storage::{Map, PrefixBound, Set},
};

pub const NODES: Map<(u64, &BitArray), Node> = Map::new("n");

pub const ORPHANS: Set<(u64, u64, &BitArray)> = Set::new("o");

/// The bit path of the root node, which is just empty
pub const ROOT_BITS: BitArray = BitArray::new_empty();

/// Describes what happens after applying ops (a slice of `HashedPair`) at a
/// node and its subtree.
#[derive(Debug)]
enum Outcome {
    /// No change happend to the node. Return `Some(node)` if the node existed,
    /// `None` if the node didn't exist.
    Unchanged(Option<Node>),
    /// The node has been changed. Return the changed node.
    Updated(Node),
    /// The node used to exist but has now been deleted.
    Deleted,
}

/// Jellyfish Merkle tree (JMT).
///
/// Adapted from Diem's work:
/// - Whitepaper:
///   <https://developers.diem.com/docs/technical-papers/jellyfish-merkle-tree-paper/>
/// - Rust implementation:
///   <https://github.com/diem/diem/tree/latest/storage/jellyfish-merkle>
///
/// Also worth looking into:
/// - Penumbra's adaptation:
///   <https://github.com/penumbra-zone/jmt>
/// - Sovereign Lab's article on optimizations:
///   <https://mirror.xyz/sovlabs.eth/jfx_cJ_15saejG9ZuQWjnGnG-NfahbazQH98i1J3NN8>
pub struct MerkleTree;

impl Commitment for MerkleTree {
    type Proof = Proof;

    /// Get the root hash at the given version. Use latest version if unspecified.
    ///
    /// If the root node is not found at the version, return None. There are two
    /// possible reasons that it's not found: either no data has ever been
    /// written to the tree yet, or the version is old and has been pruned.
    fn root_hash(storage: &dyn Storage, version: u64) -> StdResult<Option<Hash256>> {
        let root_node = NODES.may_load(storage, (version, &ROOT_BITS))?;
        Ok(root_node.map(|node| node.hash()))
    }

    /// Apply a batch of ops to the tree. Return the new root hash.
    ///
    /// If the tree isn't changed, the version isn't incremented.
    ///
    /// If the tree becomes empty after applying the ops, `None` is returned as
    /// the new root.
    ///
    /// This function takes a batch where both the keys and values are prehashes.
    /// If you already have them hashed and sorted ascendingly by the key hashes,
    /// use `apply` instead.
    fn apply(
        storage: &mut dyn Storage,
        old_version: u64,
        new_version: u64,
        batch: &Batch,
    ) -> StdResult<Option<Hash256>> {
        // Hash256 the keys and values
        let mut batch: Vec<_> = batch
            .iter()
            .map(|(k, op)| (k.sha2_256(), op.as_ref().map(|v| v.sha2_256())))
            .collect();

        // Sort by key hashes ascendingly
        batch.sort_by_key(|(k1, _)| *k1);

        // Apply the hashed keys and values
        apply(storage, old_version, new_version, batch)
    }

    /// Generate Merkle proof for the a key at the given version.
    ///
    /// Notes:
    ///
    /// - If the key exists in the tree, a membership proof is returned;
    ///   otherwise, a non-membership proof is returned.
    /// - If version is isn't specified, use the latest version.
    /// - If the tree is empty, a "data not found" error is returned.
    ///
    /// Note that this method only looks at the key, not the value. Therefore
    /// it may be possible that the caller thinks key A exists with value B,
    /// while in fact it exists with value C, in which case this method will
    /// succeed, returning a proof for the membership of (A, C). If the caller
    /// then attempts to verify the proof with (A, B) it will fail.
    ///
    /// The intended way to avoid this situation is to use a raw key-value store
    /// together with the Merkle tree:
    ///
    /// - raw KV store stores prehash keys + prehash values
    /// - Merkle tree stores hashed keys + hashed values
    ///
    /// To query a key with proof, the caller should first call `get` on the raw
    /// KV store, then `prove` on the Merkle tree. This separation of data
    /// store and data commitment was put forward by Cosmos SDK's ADR-65:
    /// <https://github.com/cosmos/cosmos-sdk/blob/main/docs/architecture/adr-065-store-v2.md>
    fn prove(storage: &dyn Storage, key_hash: Hash256, version: u64) -> StdResult<Proof> {
        let mut bits = ROOT_BITS;
        let bitarray = BitArray::from_bytes(&key_hash);
        let mut iter = bitarray.range(None, None, Order::Ascending);
        let mut node = NODES.load(storage, (version, &bits))?;
        let mut proof_node = None;
        let mut sibling_hashes = vec![];

        loop {
            match node {
                // We've reached a leaf node. If the key hashes match then we've
                // found it. If they don't match then we know the key doesn't
                // doesn't exist in the tree. Either way, break the loop.
                Node::Leaf(leaf) => {
                    if key_hash != leaf.key_hash {
                        proof_node = Some(ProofNode::Leaf {
                            key_hash: leaf.key_hash,
                            value_hash: leaf.value_hash,
                        });
                    }
                    break;
                },
                // We've reached an internal node. Move on to its child based on
                // the next bit in the key hash. append its sibling to the sibling
                // hashes.
                Node::Internal(InternalNode {
                    left_child,
                    right_child,
                }) => {
                    match (iter.next(), left_child, right_child) {
                        (Some(0), Some(child), sibling) => {
                            sibling_hashes.push(hash_of(sibling));
                            bits.push(0);
                            node = NODES.load(storage, (child.version, &bits))?;
                        },
                        (Some(1), sibling, Some(child)) => {
                            sibling_hashes.push(hash_of(sibling));
                            bits.push(1);
                            node = NODES.load(storage, (child.version, &bits))?;
                        },
                        (Some(0), None, sibling) => {
                            proof_node = Some(ProofNode::Internal {
                                left_hash: None,
                                right_hash: hash_of(sibling),
                            });
                            break;
                        },
                        (Some(1), sibling, None) => {
                            proof_node = Some(ProofNode::Internal {
                                left_hash: hash_of(sibling),
                                right_hash: None,
                            });
                            break;
                        },
                        (bit, ..) => {
                            // The next bit must exist, because if we have reached the end of the
                            // bitarray, the node is definitely a leaf. Also it can only be 0 or 1.
                            unreachable!("unexpected next bit: {bit:?}");
                        },
                    };
                },
            }
        }

        // In our proof format, the sibling hashes are from bottom up (from leaf
        // to the root), so we have to reverse it.
        // We can either reverse it during proving, or during verification. We
        // do it here since proving is usually done off-chain (e.g. a client
        // querying the node) while verification is usually done on-chain
        // (e.g. inside a light client).
        sibling_hashes.reverse();

        if let Some(node) = proof_node {
            Ok(Proof::NonMembership(NonMembershipProof {
                node,
                sibling_hashes,
            }))
        } else {
            Ok(Proof::Membership(MembershipProof { sibling_hashes }))
        }
    }

    /// Delete nodes that are no longer part of the tree as of `up_to_version`.
    ///
    /// Note: We must make sure `up_to_version` is smaller or equal to the
    /// latest version. We assert this in `DiskDb::prune`.
    fn prune(storage: &mut dyn Storage, up_to_version: u64) -> StdResult<()> {
        // Find all nodes that have been orphaned prior or at the `up_to_version`.
        let orphans = ORPHANS
            .prefix_range(
                storage,
                None,
                Some(PrefixBound::Inclusive(up_to_version)),
                Order::Ascending,
            )
            .collect::<StdResult<Vec<_>>>()?;

        // Delete the nodes
        for (_, version, bits) in orphans {
            NODES.remove(storage, (version, &bits));
        }

        // Delete the orphan records
        ORPHANS.prefix_clear(storage, None, Some(PrefixBound::Inclusive(up_to_version)));

        Ok(())
    }
}

// Apply a batch of ops to the tree. Return the new root hash.
/// If the tree isn't changed, the version isn't incremented.
///
/// If the tree becomes empty after applying the ops, `None` is returned as
/// the new root.
///
/// This function takes a `HashedBatch` where both the keys and values are
/// hashed, and sorted ascendingly by the key hashes. If you have a batch
/// of prehashes, use `apply_raw` instead.
fn apply(
    storage: &mut dyn Storage,
    old_version: u64,
    new_version: u64,
    batch: Vec<(Hash256, Op<Hash256>)>,
) -> StdResult<Option<Hash256>> {
    // The caller must make sure that versions are strictly incremental.
    // We assert this in debug mode must skip in release to save some time...
    debug_assert!(
        new_version == 0 || new_version > old_version,
        "version is not incremental"
    );

    // If an old root node exists (i.e. tree isn't empty at the old version),
    // mark it as orphaned.
    if NODES.has(storage, (old_version, &ROOT_BITS)) {
        ORPHANS.insert(storage, (new_version, old_version, &ROOT_BITS))?;
    }

    // Recursively apply the ops, starting at the old root.
    match apply_at(storage, new_version, old_version, ROOT_BITS, batch)? {
        // If the new tree is non-empty (i.e. it has a root node), save this
        // new root node and return its hash.
        Outcome::Updated(new_root_node) | Outcome::Unchanged(Some(new_root_node)) => {
            NODES.save(storage, (new_version, &ROOT_BITS), &new_root_node)?;
            Ok(Some(new_root_node.hash()))
        },
        // The new tree is empty. do nothing and just return `None`.
        Outcome::Deleted | Outcome::Unchanged(None) => Ok(None),
    }
}

fn apply_at(
    storage: &mut dyn Storage,
    new_version: u64,
    old_version: u64,
    bits: BitArray,
    batch: Vec<(Hash256, Op<Hash256>)>,
) -> StdResult<Outcome> {
    match NODES.may_load(storage, (old_version, &bits))? {
        Some(Node::Leaf(leaf_node)) => apply_at_leaf(storage, new_version, bits, leaf_node, batch),
        Some(Node::Internal(internal_node)) => {
            apply_at_internal(storage, new_version, bits, internal_node, batch)
        },
        None => {
            let (batch, op) = prepare_batch_for_subtree(batch, None);
            debug_assert!(op.is_none());
            create_subtree(storage, new_version, bits, batch, None)
        },
    }
}

fn apply_at_internal(
    storage: &mut dyn Storage,
    new_version: u64,
    bits: BitArray,
    mut internal_node: InternalNode,
    batch: Vec<(Hash256, Op<Hash256>)>,
) -> StdResult<Outcome> {
    // Split the batch into two, one for left child, one for right.
    let (batch_for_left, batch_for_right) = partition_batch(batch, bits);

    // Apply the left batch at left child
    let left_bits = bits.extend_one_bit(true);
    let left_outcome = apply_at_child(
        storage,
        new_version,
        left_bits,
        internal_node.left_child,
        batch_for_left,
    )?;

    // Apply the right batch at right child
    let right_bits = bits.extend_one_bit(false);
    let right_outcome = apply_at_child(
        storage,
        new_version,
        right_bits,
        internal_node.right_child,
        batch_for_right,
    )?;

    // If the left child exists and have been updated or deleted, then the
    // old one needs to be marked as orphaned.
    if let (Outcome::Updated(_) | Outcome::Deleted, Some(left_child)) =
        (&left_outcome, &internal_node.left_child)
    {
        ORPHANS.insert(storage, (new_version, left_child.version, &left_bits))?;
    }

    // If the right child exists and have been updated or deleted, then the
    // old one needs to be marked as orphaned.
    if let (Outcome::Updated(_) | Outcome::Deleted, Some(right_child)) =
        (&right_outcome, &internal_node.right_child)
    {
        ORPHANS.insert(storage, (new_version, right_child.version, &right_bits))?;
    }

    match (left_outcome, right_outcome) {
        // Neither children is changed. This node is unchanged as well.
        (Outcome::Unchanged(_), Outcome::Unchanged(_)) => {
            Ok(Outcome::Unchanged(Some(Node::Internal(internal_node))))
        },
        // Both children are deleted or never existed. Delete this node as well.
        (
            Outcome::Deleted | Outcome::Unchanged(None),
            Outcome::Deleted | Outcome::Unchanged(None),
        ) => Ok(Outcome::Deleted),
        // Left child is a leaf, right child is deleted.
        // Delete the current internal node and move left child up.
        // The child needs to marked as orphaned.
        (Outcome::Updated(left), Outcome::Deleted | Outcome::Unchanged(None)) if left.is_leaf() => {
            Ok(Outcome::Updated(left))
        },
        (Outcome::Unchanged(Some(left)), Outcome::Deleted) if left.is_leaf() => {
            // Mark left child as orphaned
            ORPHANS.insert(
                storage,
                (
                    new_version,
                    internal_node.left_child.unwrap().version,
                    &left_bits,
                ),
            )?;

            Ok(Outcome::Updated(left))
        },
        // Left child is deleted, right child is a leaf.
        // Delete the current internal node and move right child up.
        // The child needs to marked as orphaned.
        (Outcome::Deleted | Outcome::Unchanged(None), Outcome::Updated(right))
            if right.is_leaf() =>
        {
            Ok(Outcome::Updated(right))
        },
        (Outcome::Deleted, Outcome::Unchanged(Some(right))) if right.is_leaf() => {
            // Mark right child as orphaned
            ORPHANS.insert(
                storage,
                (
                    new_version,
                    internal_node.right_child.unwrap().version,
                    &right_bits,
                ),
            )?;

            Ok(Outcome::Updated(right))
        },
        // At least one child is updated and the path can't be collapsed.
        // Update the currenct node and return
        (left, right) => {
            internal_node.left_child = match left {
                Outcome::Updated(node) => {
                    NODES.save(storage, (new_version, &left_bits), &node)?;

                    Some(Child {
                        version: new_version,
                        hash: node.hash(),
                    })
                },
                Outcome::Deleted => None,
                Outcome::Unchanged(_) => internal_node.left_child,
            };

            internal_node.right_child = match right {
                Outcome::Updated(node) => {
                    NODES.save(storage, (new_version, &right_bits), &node)?;

                    Some(Child {
                        version: new_version,
                        hash: node.hash(),
                    })
                },
                Outcome::Deleted => None,
                Outcome::Unchanged(_) => internal_node.right_child,
            };

            Ok(Outcome::Updated(Node::Internal(internal_node)))
        },
    }
}

fn apply_at_child(
    storage: &mut dyn Storage,
    new_version: u64,
    child_bits: BitArray,
    child: Option<Child>,
    batch: Vec<(Hash256, Op<Hash256>)>,
) -> StdResult<Outcome> {
    match (batch.is_empty(), child) {
        // Child doesn't exist, and there is no op to apply.
        (true, None) => Ok(Outcome::Unchanged(None)),
        // Child exists, but there is no op to apply.
        (true, Some(child)) => {
            let child_node = NODES.load(storage, (child.version, &child_bits))?;
            Ok(Outcome::Unchanged(Some(child_node)))
        },
        // Child doesn't exist, but there are ops to apply.
        (false, None) => {
            let (batch, op) = prepare_batch_for_subtree(batch, None);
            debug_assert!(op.is_none());
            create_subtree(storage, new_version, child_bits, batch, None)
        },
        // Child exists, and there are ops to apply.
        (false, Some(child)) => apply_at(storage, new_version, child.version, child_bits, batch),
    }
}

fn apply_at_leaf(
    storage: &mut dyn Storage,
    new_version: u64,
    bits: BitArray,
    mut leaf_node: LeafNode,
    batch: Vec<(Hash256, Op<Hash256>)>,
) -> StdResult<Outcome> {
    let (batch, op) = prepare_batch_for_subtree(batch, Some(leaf_node));
    match (batch.is_empty(), op) {
        (true, Some(Op::Insert(value_hash))) => {
            if value_hash == leaf_node.value_hash {
                // Overwriting with the same value hash, no-op.
                Ok(Outcome::Unchanged(Some(Node::Leaf(leaf_node))))
            } else {
                leaf_node.value_hash = value_hash;
                Ok(Outcome::Updated(Node::Leaf(leaf_node)))
            }
        },
        (true, Some(Op::Delete)) => Ok(Outcome::Deleted),
        (true, None) => Ok(Outcome::Unchanged(Some(Node::Leaf(leaf_node)))),
        (false, Some(Op::Insert(value_hash))) => {
            leaf_node.value_hash = value_hash;
            create_subtree(storage, new_version, bits, batch, Some(leaf_node))
        },
        (false, Some(Op::Delete)) => create_subtree(storage, new_version, bits, batch, None),
        (false, None) => create_subtree(storage, new_version, bits, batch, Some(leaf_node)),
    }
}

fn create_subtree(
    storage: &mut dyn Storage,
    version: u64,
    bits: BitArray,
    batch: Vec<(Hash256, Hash256)>,
    existing_leaf: Option<LeafNode>,
) -> StdResult<Outcome> {
    match (batch.len(), existing_leaf) {
        // The subtree to be created is empty: do nothing.
        (0, None) => Ok(Outcome::Unchanged(None)),
        // The subtree to be created contains exactly one node, which is an
        // existing leaf node.
        (0, Some(leaf_node)) => Ok(Outcome::Unchanged(Some(Node::Leaf(leaf_node)))),
        // The subtree to be created contains exactly one node, which is a
        // new leaf node.
        // This case requires special attention: we don't save the node yet,
        // because the path may be collapsed if its sibling gets deleted.
        (1, None) => {
            let (key_hash, value_hash) = only_item(batch);
            Ok(Outcome::Updated(Node::Leaf(LeafNode {
                key_hash,
                value_hash,
            })))
        },
        // The subtree to be created contains more 2 or more nodes.
        // Recursively create the tree. Return the subtree's root, an
        // internal node.
        // Note that in this scenario, we certainly don't need to collapse the
        // path.
        (_, existing_leaf) => {
            // Split the batch for left and right children.
            let (batch_for_left, batch_for_right) = partition_batch(batch, bits);
            let (leaf_for_left, leaf_for_right) = partition_leaf(existing_leaf, bits);

            // Create the left subtree.
            let left_bits = bits.extend_one_bit(true);
            let left_outcome =
                create_subtree(storage, version, left_bits, batch_for_left, leaf_for_left)?;

            // Create the right subtree.
            let right_bits = bits.extend_one_bit(false);
            let right_outcome = create_subtree(
                storage,
                version,
                right_bits,
                batch_for_right,
                leaf_for_right,
            )?;

            // If a subtree is non-empty, save it's root node.
            if let Outcome::Updated(node) | Outcome::Unchanged(Some(node)) = &left_outcome {
                NODES.save(storage, (version, &left_bits), node)?;
            }
            if let Outcome::Updated(node) | Outcome::Unchanged(Some(node)) = &right_outcome {
                NODES.save(storage, (version, &right_bits), node)?;
            }

            Ok(Outcome::Updated(Node::Internal(InternalNode {
                left_child: into_child(version, left_outcome),
                right_child: into_child(version, right_outcome),
            })))
        },
    }
}

#[inline]
fn partition_batch<T>(
    mut batch: Vec<(Hash256, T)>,
    bits: BitArray,
) -> (Vec<(Hash256, T)>, Vec<(Hash256, T)>) {
    let partition_point =
        batch.partition_point(|(key_hash, _)| bit_at_index(key_hash, bits.num_bits) == 0);
    let right = batch.split_off(partition_point);
    (batch, right)
}

#[inline]
fn partition_leaf(leaf: Option<LeafNode>, bits: BitArray) -> (Option<LeafNode>, Option<LeafNode>) {
    if let Some(leaf) = leaf {
        let bit = bit_at_index(&leaf.key_hash, bits.num_bits);
        // 0 = left, 1 = right
        debug_assert!(bit == 0 || bit == 1);
        if bit == 0 {
            (Some(leaf), None)
        } else {
            (None, Some(leaf))
        }
    } else {
        (None, None)
    }
}

/// Given a batch,
/// 1. See if there is an op whose key hash matches the existing leaf's. If yes,
///    take it out.
/// 2. Amoung the rest ops, filter off the deletes, keeping only the inserts.
#[inline]
fn prepare_batch_for_subtree(
    batch: Vec<(Hash256, Op<Hash256>)>,
    existing_leaf: Option<LeafNode>,
) -> (Vec<(Hash256, Hash256)>, Option<Op<Hash256>>) {
    let mut maybe_op = None;
    let filtered_batch = batch
        .into_iter()
        .filter_map(|(key_hash, op)| {
            // check if key hash match the leaf's
            if let Some(leaf) = existing_leaf
                && key_hash == leaf.key_hash
            {
                maybe_op = Some(op);
                return None;
            }

            // keep inserts, remove deletes
            if let Op::Insert(value_hash) = op {
                Some((key_hash, value_hash))
            } else {
                None
            }
        })
        .collect();
    (filtered_batch, maybe_op)
}

/// Consume a vector, assert it has exactly item, return this item by value.
#[inline]
fn only_item<T>(mut vec: Vec<T>) -> T {
    debug_assert_eq!(vec.len(), 1);
    vec.pop().unwrap()
}

/// Get the i-th bit without having to cast the byte slice to BitArray (which
/// involves some copying).
#[inline]
fn bit_at_index(bytes: &[u8], index: usize) -> u8 {
    let (quotient, remainder) = (index / 8, index % 8);
    let byte = bytes[quotient];
    (byte >> (7 - remainder)) & 0b1
}

#[inline]
fn hash_of(child: Option<Child>) -> Option<Hash256> {
    child.map(|child| child.hash)
}

#[inline]
fn into_child(version: u64, outcome: Outcome) -> Option<Child> {
    match outcome {
        Outcome::Updated(node) | Outcome::Unchanged(Some(node)) => Some(Child {
            version,
            hash: node.hash(),
        }),
        Outcome::Unchanged(None) => None,
        Outcome::Deleted => unreachable!("invalid outcome when building subtree: {outcome:?}"),
    }
}

// ----------------------------------- tests -----------------------------------

// we use the following very simple merkle tree in these tests:
// (parentheses designates internal nodes. without parentheses then it's a leaf)
//
//           root
//         ┌──┴──┐
//        (0)    1
//      ┌──┴──┐
//    null   (01)
//         ┌──┴──┐
//        010  (011)
//            ┌──┴──┐
//          0110   0111
//
// to build this tree, we need four keys that hash to 010..., 0110..., 0111...,
// and 1... respectively, which were found with a little trials:
//
// sha256("r") = 0100...
// sha256("m") = 0110...
// sha256("L") = 0111...
// sha256("a") = 1100...
//
// the node hashes are computed as follows:
//
// hash of node 0110
// = sha256(01 | sha256("m") | sha256("bar"))
// = sha256(01 | 62c66a7a5dd70c3146618063c344e531e6d4b59e379808443ce962b3abd63c5a | fcde2b2edba56bf408601fb721fe9b5c338d10ee429ea04fae5511b68fbf8fb9)
// = fd34e3f8d9840e7f6d6f639435b6f9b67732fc5e3d5288e268021aeab873f280
//
// hash of node 0111
// = sha256(01 | sha256("L") | sha256("fuzz"))
// = sha256(01 | 72dfcfb0c470ac255cde83fb8fe38de8a128188e03ea5ba5b2a93adbea1062fa | 93850b707585e404e4951a3ddc1f05a34b3d4f5fc081d616f46d8a2e8f1c8e68)
// = 412341380b1e171077dd9da9af936ae2126ede2dd91dc5acb0f77363d46eb76b
//
// hash of node 011
// = sha256(00 | fd34e3f8d9840e7f6d6f639435b6f9b67732fc5e3d5288e268021aeab873f280 | 412341380b1e171077dd9da9af936ae2126ede2dd91dc5acb0f77363d46eb76b)
// = e104e2bcf24027af737c021033cb9d8cbd710a463f54ae6f2ff9eb06c784c744
//
// hash of node 010
// = sha256(01 | sha256("r") | sha256("foo"))
// = sha256(01 | 454349e422f05297191ead13e21d3db520e5abef52055e4964b82fb213f593a1 | 2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae)
// = c8348e9a7a327e8b76e97096c362a1f87071ee4108b565d1f409529c189cb684
//
// hash of node 01
// = sha256(00 | c8348e9a7a327e8b76e97096c362a1f87071ee4108b565d1f409529c189cb684 | e104e2bcf24027af737c021033cb9d8cbd710a463f54ae6f2ff9eb06c784c744)
// = 521de0a3ef2b7791666435a872ca9ec402ce886aff07bb4401de28bfdde4a13b
//
// hash of node 0
// = sha256(00 | 0000000000000000000000000000000000000000000000000000000000000000 | 521de0a3ef2b7791666435a872ca9ec402ce886aff07bb4401de28bfdde4a13b)
// = b843a96765fc40641227234e9f9a2736c2e0cdf8fb2dc54e358bb4fa29a61042
//
// hash of node 1
// = sha256(01 | sha256("a") | sha256("buzz"))
// = sha256(01 | ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb | 9fff3bcb10ca5e87b8109ccde9e9452012d634a005942afc46cf2b7fa307526a)
// = cb640e68682628445a3e0713fafe91b9cefe4f81c2337e9d3df201d81ae70222
//
// root hash
// = sha256(00 | b843a96765fc40641227234e9f9a2736c2e0cdf8fb2dc54e358bb4fa29a61042 | cb640e68682628445a3e0713fafe91b9cefe4f81c2337e9d3df201d81ae70222)
// = ae08c246d53a8ff3572a68d5bba4d610aaaa765e3ef535320c5653969aaa031b

#[cfg(test)]
mod tests {
    use {
        super::*,
        dango_primitives::{MembershipProof, MockStorage, NonMembershipProof, ResultExt, StdError},
        hex_literal::hex,
        test_case::test_case,
    };

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

    fn build_test_case() -> StdResult<(MockStorage, Option<Hash256>)> {
        let mut storage = MockStorage::new();
        let root_hash = MerkleTree::apply(
            &mut storage,
            0,
            0,
            &Batch::from([
                (b"r".to_vec(), Op::Insert(b"foo".to_vec())),
                (b"m".to_vec(), Op::Insert(b"bar".to_vec())),
                (b"L".to_vec(), Op::Insert(b"fuzz".to_vec())),
                (b"a".to_vec(), Op::Insert(b"buzz".to_vec())),
            ]),
        )?;
        Ok((storage, root_hash))
    }

    #[test]
    fn applying_initial_batch() {
        let (storage, root_hash) = build_test_case().unwrap();
        assert_eq!(root_hash, Some(HASH_ROOT));
        assert!(
            ORPHANS
                .range(&storage, None, None, Order::Ascending)
                .next()
                .is_none()
        );
    }

    // Delete the leaves 010 and 0110. this should cause the leaf 0111 be moved
    // up to bit path `0`. the result tree is:
    //
    //           root
    //         ┌──┴──┐
    //         0     1
    //
    // hash of node 0
    // = 412341380b1e171077dd9da9af936ae2126ede2dd91dc5acb0f77363d46eb76b
    // (the same as that of node 0111 of the last version)
    //
    // hash of node 1
    // = cb640e68682628445a3e0713fafe91b9cefe4f81c2337e9d3df201d81ae70222
    // (unchanged)
    //
    // root hash
    // = sha256(00 | 412341380b1e171077dd9da9af936ae2126ede2dd91dc5acb0f77363d46eb76b | cb640e68682628445a3e0713fafe91b9cefe4f81c2337e9d3df201d81ae70222)
    // = b3e4002b2d95d57ab44bbf64c8cfb04904c02fb2df9c859a75d82b02fd087dbf
    #[test]
    fn collapsing_path() {
        let (mut storage, _) = build_test_case().unwrap();
        let new_root_hash = MerkleTree::apply(
            &mut storage,
            0,
            1,
            &Batch::from([(b"r".to_vec(), Op::Delete), (b"m".to_vec(), Op::Delete)]),
        )
        .unwrap();
        assert_eq!(
            new_root_hash,
            Some(Hash256::from_inner(hex!(
                "b3e4002b2d95d57ab44bbf64c8cfb04904c02fb2df9c859a75d82b02fd087dbf"
            )))
        );
    }

    // Try deleting every single node. the function should return None as the
    // new root hash. See that nodes have been properly marked as orphaned.
    #[test]
    fn deleting_all_nodes() {
        let (mut storage, _) = build_test_case().unwrap();

        // Check that new root hash is `None`.
        let new_root_hash = MerkleTree::apply(
            &mut storage,
            0,
            1,
            &Batch::from([
                (b"r".to_vec(), Op::Delete),
                (b"m".to_vec(), Op::Delete),
                (b"L".to_vec(), Op::Delete),
                (b"a".to_vec(), Op::Delete),
            ]),
        )
        .unwrap();
        assert!(new_root_hash.is_none());

        // Check that every node has been marked as orphaned.
        for item in NODES.keys(&storage, None, None, Order::Ascending) {
            let (version, bits) = item.unwrap();
            assert_eq!(version, 0);
            assert!(ORPHANS.has(&storage, (1, version, &bits)));
        }
    }

    // No-op is when the batch contains entirely of overwrites of values by the
    // same value, or deletes of non-existing keys. The version number shouldn't
    // be incremented and root hash shouldn't be changed.
    #[test]
    fn no_ops() {
        let (mut storage, _) = build_test_case().unwrap();

        let new_root_hash = MerkleTree::apply(
            &mut storage,
            0,
            1,
            &Batch::from([
                // overwriting keys with the same keys
                (b"r".to_vec(), Op::Insert(b"foo".to_vec())),
                (b"m".to_vec(), Op::Insert(b"bar".to_vec())),
                (b"L".to_vec(), Op::Insert(b"fuzz".to_vec())),
                (b"a".to_vec(), Op::Insert(b"buzz".to_vec())),
                // deleting non-existing keys
                (b"larry".to_vec(), Op::Delete), // 00001101...
                (b"trump".to_vec(), Op::Delete), // 10100110...
                (b"biden".to_vec(), Op::Delete), // 00000110...
            ]),
        )
        .unwrap();

        // Make sure the root hash is unchanged
        assert_eq!(new_root_hash, Some(HASH_ROOT));

        // Make sure that no node has been marked as orphaned (other than the
        // old root node, which is always orphaned).
        for item in ORPHANS.range(&storage, None, None, Order::Ascending) {
            let (orphaned_since_version, version, bits) = item.unwrap();
            assert_eq!(orphaned_since_version, 1);
            assert_eq!(version, 0);
            assert_eq!(bits, ROOT_BITS);
        }

        // Make sure no node of version 1 has been written (other than the new
        // root node, which is always written).
        for item in NODES.keys(&storage, None, None, Order::Ascending) {
            let (version, bits) = item.unwrap();
            assert!(version == 0 || (version == 1 && bits == ROOT_BITS));
        }
    }

    #[test_case(
        "r",
        Proof::Membership(MembershipProof {
            sibling_hashes: vec![
                Some(HASH_011),
                None,
                Some(HASH_1),
            ],
        });
        "proving membership of r"
    )]
    #[test_case(
        "m",
        Proof::Membership(MembershipProof {
            sibling_hashes: vec![
                Some(HASH_0111),
                Some(HASH_010),
                None,
                Some(HASH_1),
            ],
        });
        "proving membership of m"
    )]
    #[test_case(
        "L",
        Proof::Membership(MembershipProof {
            sibling_hashes: vec![
                Some(HASH_0110),
                Some(HASH_010),
                None,
                Some(HASH_1),
            ],
        });
        "proving membership of L"
    )]
    #[test_case(
        "a",
        Proof::Membership(MembershipProof {
            sibling_hashes: vec![Some(HASH_0)],
        });
        "proving membership of a"
    )]
    #[test_case(
        "b", // sha256("b") = 0011... node 0 doesn't have a left child
        Proof::NonMembership(NonMembershipProof {
            node: ProofNode::Internal {
                left_hash:  None,
                right_hash: Some(HASH_01),
            },
            sibling_hashes: vec![Some(HASH_1)],
        });
        "proving non-membership of b"
    )]
    #[test_case(
        "o", // sha256("o") = 011001... there's a leaf 0110 ("m") which doesn't match key
        Proof::NonMembership(NonMembershipProof {
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
        });
        "proving non-membership of o"
    )]
    fn proving(key: &str, proof: Proof) {
        let (storage, _) = build_test_case().unwrap();
        assert_eq!(
            MerkleTree::prove(&storage, key.as_bytes().sha2_256(), 0).unwrap(),
            proof
        );
    }

    /// An edge case found in the Zellic audit.
    ///
    /// Attempting to generate proofs in an empty tree would fail with a "data
    /// not found" error as the root node doesn't exist.
    ///
    /// We decide to simply add documentation and not fix this, as fixing it
    /// involves changing the function signature, which impacts many downstream
    /// packages. It's too big of a change for a minor edge case we consider it
    /// not worth it.
    ///
    /// In practice, the tree is never empty, as we always need to store things
    /// like the chain ID and config.
    #[test]
    fn proving_in_empty_tree() {
        let storage = MockStorage::new();

        // Attempting to generate proof without applying any batch.
        // The tree is empty at this point.
        // We check that the expected error is emitted.
        MerkleTree::prove(&storage, b"foo".sha2_256(), 0).should_fail_with_error(
            StdError::data_not_found::<Node>(NODES.path((0, &ROOT_BITS)).storage_key()),
        );
    }

    #[test]
    fn pruning() {
        let (mut storage, _) = build_test_case().unwrap();

        // Do a few batches. For simplicity, we just delete one nodes each version.
        // v1
        MerkleTree::apply(
            &mut storage,
            0,
            1,
            &Batch::from([(b"m".to_vec(), Op::Delete)]),
        )
        .unwrap();

        // v2
        MerkleTree::apply(
            &mut storage,
            1,
            2,
            &Batch::from([(b"r".to_vec(), Op::Delete)]),
        )
        .unwrap();

        // v3
        MerkleTree::apply(
            &mut storage,
            2,
            3,
            &Batch::from([(b"L".to_vec(), Op::Delete)]),
        )
        .unwrap();

        // v4
        MerkleTree::apply(
            &mut storage,
            3,
            4,
            &Batch::from([(b"a".to_vec(), Op::Delete)]),
        )
        .unwrap();

        // Before doing any pruning, check nodes and orphans are correct.
        assert_tree(
            &storage,
            [
                (0, ROOT_BITS),
                (0, BitArray::from_bits(&[0])),
                (0, BitArray::from_bits(&[1])),
                (0, BitArray::from_bits(&[0, 1])),
                (0, BitArray::from_bits(&[0, 1, 0])),
                (0, BitArray::from_bits(&[0, 1, 1])),
                (0, BitArray::from_bits(&[0, 1, 1, 0])),
                (0, BitArray::from_bits(&[0, 1, 1, 1])),
                (1, ROOT_BITS),
                (1, BitArray::from_bits(&[0])),
                (1, BitArray::from_bits(&[0, 1])),
                (1, BitArray::from_bits(&[0, 1, 1])),
                (2, ROOT_BITS),
                (2, BitArray::from_bits(&[0])),
                (3, ROOT_BITS),
                // v5 tree is empty
            ],
            [
                (1, 0, ROOT_BITS),
                (1, 0, BitArray::from_bits(&[0])),
                (1, 0, BitArray::from_bits(&[0, 1])),
                (1, 0, BitArray::from_bits(&[0, 1, 1])),
                (1, 0, BitArray::from_bits(&[0, 1, 1, 0])),
                (1, 0, BitArray::from_bits(&[0, 1, 1, 1])),
                (2, 0, BitArray::from_bits(&[0, 1, 0])),
                (2, 1, ROOT_BITS),
                (2, 1, BitArray::from_bits(&[0])),
                (2, 1, BitArray::from_bits(&[0, 1])),
                (2, 1, BitArray::from_bits(&[0, 1, 1])),
                (3, 0, BitArray::from_bits(&[1])),
                (3, 2, ROOT_BITS),
                (3, 2, BitArray::from_bits(&[0])),
                (4, 3, ROOT_BITS),
            ],
        );

        // Prune up to v1
        MerkleTree::prune(&mut storage, 1).unwrap();
        assert_tree(
            &storage,
            [
                (0, BitArray::from_bits(&[1])),
                (0, BitArray::from_bits(&[0, 1, 0])),
                (1, ROOT_BITS),
                (1, BitArray::from_bits(&[0])),
                (1, BitArray::from_bits(&[0, 1])),
                (1, BitArray::from_bits(&[0, 1, 1])),
                (2, ROOT_BITS),
                (2, BitArray::from_bits(&[0])),
                (3, ROOT_BITS),
            ],
            [
                (2, 0, BitArray::from_bits(&[0, 1, 0])),
                (2, 1, ROOT_BITS),
                (2, 1, BitArray::from_bits(&[0])),
                (2, 1, BitArray::from_bits(&[0, 1])),
                (2, 1, BitArray::from_bits(&[0, 1, 1])),
                (3, 0, BitArray::from_bits(&[1])),
                (3, 2, ROOT_BITS),
                (3, 2, BitArray::from_bits(&[0])),
                (4, 3, ROOT_BITS),
            ],
        );

        // Prune up to v2
        MerkleTree::prune(&mut storage, 2).unwrap();
        assert_tree(
            &storage,
            [
                (0, BitArray::from_bits(&[1])),
                (2, ROOT_BITS),
                (2, BitArray::from_bits(&[0])),
                (3, ROOT_BITS),
            ],
            [
                (3, 0, BitArray::from_bits(&[1])),
                (3, 2, ROOT_BITS),
                (3, 2, BitArray::from_bits(&[0])),
                (4, 3, ROOT_BITS),
            ],
        );

        // Prune up to v3
        MerkleTree::prune(&mut storage, 3).unwrap();
        assert_tree(&storage, [(3, ROOT_BITS)], [(4, 3, ROOT_BITS)]);

        // Prune up to v4
        MerkleTree::prune(&mut storage, 4).unwrap();
        assert_tree(&storage, [], []);
    }

    #[test]
    fn no_extra_saves() {
        let mut storage = MockStorage::new();

        let _ = MerkleTree::apply(
            &mut storage,
            0,
            1,
            &Batch::from([
                (b"m".to_vec(), Op::Insert(b"10".to_vec())),
                (b"L".to_vec(), Op::Insert(b"6".to_vec())),
                (b"q".to_vec(), Op::Delete),
                (b"Z".to_vec(), Op::Insert(b"2".to_vec())),
                (b"a".to_vec(), Op::Insert(b"14".to_vec())),
            ]),
        );

        let _ = MerkleTree::apply(
            &mut storage,
            1,
            2,
            &Batch::from([
                (b"r".to_vec(), Op::Insert(b"6".to_vec())),
                (b"w".to_vec(), Op::Delete),
                (b"m".to_vec(), Op::Delete),
                (b"L".to_vec(), Op::Delete),
                (b"a".to_vec(), Op::Insert(b"5".to_vec())),
            ]),
        );

        assert_tree(
            &storage,
            [
                (1, ROOT_BITS),
                (1, BitArray::from_bits(&[0])),
                (1, BitArray::from_bits(&[1])),
                (1, BitArray::from_bits(&[0, 1])),
                (1, BitArray::from_bits(&[1, 0])),
                (1, BitArray::from_bits(&[1, 1])),
                (1, BitArray::from_bits(&[0, 1, 1])),
                (1, BitArray::from_bits(&[0, 1, 1, 0])),
                (1, BitArray::from_bits(&[0, 1, 1, 1])),
                (2, ROOT_BITS),
                (2, BitArray::from_bits(&[0])),
                (2, BitArray::from_bits(&[1])),
                (2, BitArray::from_bits(&[1, 1])),
                // In a previous implementation, the tree incorrectly saves the
                // node { version = 2, bits = [0, 1, 0] }, which is a duplicate
                // of { version = 2, bits = [0] }.
                // Here we verify it doesn't exist.
                // This bug was discovered in the Informal audit.
            ],
            [
                (2, 1, BitArray::from_bits(&[])),
                (2, 1, BitArray::from_bits(&[0])),
                (2, 1, BitArray::from_bits(&[1])),
                (2, 1, BitArray::from_bits(&[0, 1])),
                (2, 1, BitArray::from_bits(&[1, 1])),
                (2, 1, BitArray::from_bits(&[0, 1, 1])),
                (2, 1, BitArray::from_bits(&[0, 1, 1, 0])),
                (2, 1, BitArray::from_bits(&[0, 1, 1, 1])),
            ],
        );
    }

    fn assert_tree<const N: usize, const O: usize>(
        storage: &dyn Storage,
        nodes: [(u64, BitArray); N],
        orphans: [(u64, u64, BitArray); O],
    ) {
        let nodes_dump = NODES
            .keys(storage, None, None, Order::Ascending)
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(nodes_dump, nodes);

        let orphans_dump = ORPHANS
            .range(storage, None, None, Order::Ascending)
            .collect::<StdResult<Vec<_>>>()
            .unwrap();
        assert_eq!(orphans_dump, orphans);
    }
}
