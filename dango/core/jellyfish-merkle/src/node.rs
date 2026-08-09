use {
    borsh::{BorshDeserialize, BorshSerialize},
    dango_primitives::{Hash256, HashExt},
};

pub const INTERNAL_NODE_HASH_PREFIX: &[u8] = &[0];
pub const LEAF_NODE_HASH_PERFIX: &[u8] = &[1];

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Child {
    pub version: u64,
    pub hash: Hash256,
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct InternalNode {
    pub left_child: Option<Child>,
    pub right_child: Option<Child>,
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeafNode {
    pub key_hash: Hash256,
    pub value_hash: Hash256,
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Node {
    Internal(InternalNode),
    Leaf(LeafNode),
}

impl Node {
    pub fn is_leaf(&self) -> bool {
        match self {
            Node::Internal(_) => false,
            Node::Leaf(_) => true,
        }
    }

    /// Computing the node's hash.
    ///
    /// To distinguish internal and leaf nodes, internal nodes are prefixed with
    /// a zero byte, leaves are prefixed with a 1 byte.
    ///
    /// If an internal nodes doesn't have a left or right child, that child is
    /// represented by a zero hash `[0u8; 32]`.
    pub fn hash(self) -> Hash256 {
        match self {
            Node::Internal(InternalNode {
                left_child,
                right_child,
            }) => hash_internal_node(left_child.map(|c| c.hash), right_child.map(|c| c.hash)),
            Node::Leaf(LeafNode {
                key_hash,
                value_hash,
            }) => hash_leaf_node(key_hash, value_hash),
        }
    }
}

pub fn hash_internal_node(left_hash: Option<Hash256>, right_hash: Option<Hash256>) -> Hash256 {
    let mut preimage = Vec::with_capacity(INTERNAL_NODE_HASH_PREFIX.len() + Hash256::LENGTH * 2);
    preimage.extend_from_slice(INTERNAL_NODE_HASH_PREFIX);
    preimage.extend_from_slice(&left_hash.unwrap_or(Hash256::ZERO));
    preimage.extend_from_slice(&right_hash.unwrap_or(Hash256::ZERO));
    preimage.sha2_256()
}

pub fn hash_leaf_node(key_hash: Hash256, value_hash: Hash256) -> Hash256 {
    let mut preimage = Vec::with_capacity(INTERNAL_NODE_HASH_PREFIX.len() + Hash256::LENGTH * 2);
    preimage.extend_from_slice(LEAF_NODE_HASH_PERFIX);
    preimage.extend_from_slice(&key_hash);
    preimage.extend_from_slice(&value_hash);
    preimage.sha2_256()
}
