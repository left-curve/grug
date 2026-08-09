# Proofs

_This document was prepared by the [Informal Systems security team](https://informal.systems/security)_

This document describes how ICS23 proof generation was modelled in Quint, and how everything corresponds to the Rust implementation. In this version of the Quint model, we tried to make things as close to the Rust implementation as possible. However, since Quint does not support hashing, there were some challenges.

Most of the correspondance is shown by comparing the Rust code with Quint code short snippets at a time. The most complicated correspondance is on early returns and panics, which do not exist in Quint. We are explaining this in detail in [`ics23_prove_existence`](#ics23-proving-existence) and [`ics23_prove`](#generating-commitment-proof).

This document covers:

- [`ics23_prove_existence`](#ics23-proving-existence)
- [`leftNeighbor`](#get-left-neighbor)
- [`rightNeighbor`](#get-right-neighbor)
- [`ics23_prove`](#generating-commitment-proof)

> [!TIP]
> This markdown file contains some metadata and comments that enable it to be tangled to a full Quint file (using [lmt](https://github.com/driusan/lmt)). The Quint file can be found at [proofs.qnt](../quint/proofs.qnt).

<!-- Boilerplate: tangled from comment to avoid markdown rendering
```bluespec quint/proofs.qnt
// -*- mode: Bluespec; -*-

// Construction of proofs for Jellyfish Merke Trees
//
// Josef Widder, Informal Systems, 2024
// Aleksandar Ignjatijevic, Informal Systems, 2024
// Gabriela Moreira, Informal Systems, 2024

module proofs {
  import basicSpells.* from "./spells/basicSpells"
  import rareSpells.* from "./spells/rareSpells"
  import hashes.* from "./hashes"
  import tree.* from "./tree"
  import proof_types.* from "./proof_types"
  import node.* from "./node"
  import utils.* from "./utils"

  <<<definitions>>>
}
```
-->

<!--
```bluespec "definitions" +=
/// Returns optional list of InnerOps as a path to the leaf with particular key_hash
```
-->

## ICS23 proving existence

Like mentioned in [tree_manipulation.md](./tree_manipulation.md), Quint does not support early breaks, which meant that we had use different strategies.
First, the signature:

```rust
pub fn ics23_prove_existence(
  &self,
  storage: &dyn Storage,
  version: u64,
  key_hash: Hash256,
) -> StdResult<Vec<InnerOp>> {
```

```bluespec "definitions" +=
pure def ics23_prove_existence(t: Tree, version: Version, key_hash: BitArray)
: Option[List[InnerOp]] =
```
<!---
```bluespec "definitions" +=
  <<<ics23_prove_existence>>>
```
--->
As clearly visible, the function signature is different. In our specification, we are returning `Option[List[InnerOp]]`. Returning `None` is reserved for any error that has occured when traversing the tree. In Rust implementation, looping is done until leaf is reached. However, since Quint does not support early returns, we had to have a finite set of key prefixes that we can fold over.

```bluespec "ics23_prove_existence" +=
val prefixes_list = 0.to(key_hash.length()).map( i => key_hash.slice(0,i)).toList(listCompare)
```

We set our `iterator` to a record that stores `path` - empty list, index of `key_prefix` in a `prefixes_list` - `0`, boolean value which indicates wether leaf has been found - `false` and a `child_version` which helps us find the correct version of internal node's child - `version` for which `ics23_prove_existence` is called.

```bluespec "ics23_prove_existence" +=
val r = prefixes_list.foldl({ path: List(), i: 0, found: false, child_version: version}, (iterator, key_prefix) =>
```
<!--
```bluespec "ics23_prove_existence" +=
  <<<ics23_prove_existence_1>>>
```
-->
`iterator.found` is used in our early return workaround. If the algorithm has found the leaf with adequate `key_hash` or if it cannot find a leaf with corresponding `key_prefix` and `iterator.child_version`, iterator won't be changed until fold has finished.

```bluespec "ics23_prove_existence_1" +=
if (iterator.found or not(t.nodes.keys().contains({key_hash: key_prefix, version: iterator.child_version})))
  iterator
```
<!--
```bluespec "ics23_prove_existence_1" +=
else
  <<<ics23_prove_existence_2>>>
```
-->

In first fold iteration `key_prefix` will take `ROOT_BITS` value and root will be fetched from the state.

```bluespec "ics23_prove_existence_2" +=
val node = t.nodes.get({key_hash: key_prefix, version: iterator.child_version})
```

This corresponds to the following Rust parts of code:

```rust
let mut bits = ROOT_BITS;
(...)
let mut node = self.nodes.load(storage, (version, &bits))?;
```

After node has been fetched from the tree, we perform match operation, because node can either be `Leaf` or `Internal`. If the algorithm has reached the leaf, iterator will take value: `{ ...iterator, i: iterator.i + 1, found: l.key_hash == key_hash }`, `...iterator` being the spread operator which will take values of old iterator and apply it to new one. This means that if `key_hashes` are the same, we have found the leaf we are looking for and we should early break, setting `iterator.found` to true. However if the algorithm has reached an internal node, that means that there are more things to do to reach the leaf. Here is the `match` statement in Rust implementation, that we emulated in Quint. As previously mentioned, we will not have record of `hash` in our `InnerOp` type.

```rust
match node {
  Node::Leaf(leaf) => {
      assert_eq!(leaf.key_hash, key_hash, "target key hash not found");
      break;
  },
  Node::Internal(InternalNode {
      left_child,
      right_child,
  }) => match (iter.next(), left_child, right_child) {
      (Some(0), Some(child), sibling) => {
          bits.push(0);
          node = self.nodes.load(storage, (child.version, &bits))?;
          path.push(InnerOp {
              // Not sure why we have to include the `HashOp` here
              // when it's already in the `ProofSpec`.
              hash: ICS23_PROOF_SPEC.inner_spec.as_ref().unwrap().hash,
              prefix: INTERNAL_NODE_HASH_PREFIX.to_vec(),
              suffix: sibling.map(|c| c.hash).unwrap_or(Hash256::ZERO).to_vec(),
          });
      },
      (Some(1), sibling, Some(child)) => {
          bits.push(1);
          node = self.nodes.load(storage, (child.version, &bits))?;
          path.push(InnerOp {
              hash: ICS23_PROOF_SPEC.inner_spec.as_ref().unwrap().hash,
              prefix: [
                  INTERNAL_NODE_HASH_PREFIX,
                  sibling.map(|c| c.hash).unwrap_or(Hash256::ZERO).as_ref(),
              ]
              .concat(),
              suffix: vec![],
          })
      },
      _ => unreachable!("target key hash not found"),
  },
}
```

Since Quint's pattern matching is not as strong as Rust's we had to figure a way around it. First we will append `0` to the `key_prefix` and then try to see if, in the `prefixes_list`, the next element will have the same `key_prefix`.

```bluespec "ics23_prove_existence_2" +=
match node {
  | Leaf(l) => { ...iterator, i: iterator.i + 1,
                            found: l.key_hash == key_hash }
  | Internal(internal) =>
    val next_bit_0 = key_prefix.append(0)
    val child_version = if(prefixes_list[iterator.i + 1] == next_bit_0)
                          internal.left_child.unwrap().version
                        else
                          internal.right_child.unwrap().version
```
<!--
```bluespec "ics23_prove_existence_2" +=
    <<<ics23_prove_existence_3>>>
```
-->
Then, we will take its version and declare it `child_version`. After doing so, we check wether we should end up in the `(Some(0), Some(child), sibling)` or `(Some(1), sibling, Some(child))` `match` branch.

- If we are to end up in the `(Some(0), Some(child), sibling)` branch, that means that we will create `innerOp` variable with `prefix` being `InternalNodeHashPrefix` and `suffix` being either hash of the right child or zeroed out hash (`Hash256_ZERO`).

```bluespec "ics23_prove_existence_3" +=
val innerOp =
  if(prefixes_list[iterator.i + 1] == next_bit_0)
    { prefix: InternalNodeHashPrefix,
      suffix: match internal.right_child {
                | None => Hash256_ZERO
                | Some(c) => c.hash} }
```
<!--
```bluespec "ics23_prove_existence_3" +=
  <<<ics23_prove_existence_4>>>
```
-->
This part closely resembles this part of Rust implementation:

```rust
path.push(InnerOp {
    // Not sure why we have to include the `HashOp` here
    // when it's already in the `ProofSpec`.
    hash: ICS23_PROOF_SPEC.inner_spec.as_ref().unwrap().hash,
    prefix: INTERNAL_NODE_HASH_PREFIX.to_vec(),
    suffix: sibling.map(|c| c.hash).unwrap_or(Hash256::ZERO).to_vec(),
});
```

- If we are to end up in the `(Some(1), sibling, Some(child))` branch, that means that we will create `innerOp` variable with `prefix` being concatanated values of InternalNodeHashPrefix and either hash of the left child or zeroed out hash (`Hash256_ZERO`). In this case, `suffix` will be an empty `Map()`, which corresponds to an empty vector in Rust implementation.

```bluespec "ics23_prove_existence_4" +=
else
  { prefix: InternalNodeHashPrefix
              .termConcat(match internal.left_child {
                            | None => Hash256_ZERO
                            | Some(c) => c.hash}),
    suffix: Map() }
```
<!-- Empty line, to be tangled but not rendered
```bluespec "ics23_prove_existence_4" +=

```
-->
This part closely resembles this part of Rust implementation:

```rust
path.push(InnerOp {
  hash: ICS23_PROOF_SPEC.inner_spec.as_ref().unwrap().hash,
  prefix: [
      INTERNAL_NODE_HASH_PREFIX,
      sibling.map(|c| c.hash).unwrap_or(Hash256::ZERO).as_ref(),
  ]
  .concat(),
  suffix: vec![],
})
```

After creating the new inner op, we update `iterator` with new values, such as new index of `key_prefix`, `child_version` that we have created before creating `innerOp` and new entry in the `path` list.

```bluespec "ics23_prove_existence_3" +=
{ ...iterator, path: iterator.path.append(innerOp),
  i: iterator.i + 1, child_version: child_version }
```
<!--
```bluespec "ics23_prove_existence" +=
    }
)
```
-->
After completing the fold, we are supposed to reverse the path:

```rust
// The path goes from bottom up, so needs to be reversed.
path.reverse();
```

Our specification does the same, in the event that we have found the leaf we were looking for. Since the Rust implementation will panic when there is no leaf with the wanted `key_hash`, in order to handle panics, we decided to return `None` in the event we did not find the leaf we were looking for, and reverse the path if we did.
<!-- Empty line, to be tangled but not rendered
```bluespec "definitions" +=
```
-->
```bluespec "ics23_prove_existence" +=
if (r.found)
  Some(r.path.reverse())
else
  None
```
<!--- Empty line, to be tangled but not rendered
```bluespec "definitions" +=

```
--->

## Get left neighbor

Rust implementation of `ics23_prove` function simply looks up the state storage to find the left and right neighbors for a given key, and generate existence proof of them. Here is the Rust implementation that we emulated in Quint.

```rust
let opts = new_read_options(Some(version), None, None);
let mode = IteratorMode::From(&key_hash, Direction::Reverse);
let left = self
    .inner
    .db
    .iterator_cf_opt(&cf, opts, mode)
    .next()
    .map(|res| {
        let (_, key) = res?;
        let value = state_storage.read(&key).unwrap();
        generate_existence_proof(key.to_vec(), value)
    })
    .transpose()?;
```

In order to get the same outcome, we implemented `leftNeighbor` function.
<!---
```bluespec "definitions" +=
/// Return leaf with the largest key_hash smaller than k
```
--->
```bluespec "definitions" +=
pure def leftNeighbor(t: TreeMap, k: BitArray): Option[LeafNode] =
```
<!---
```bluespec "definitions" +=
  <<<leftNeighbor>>>
```
--->
First, we get all leaf nodes with `key_hash` smaller than the `key_hash` function parameter. For that we are calling `less_than()` function defined in the [hashes.qnt](../quint/hashes.qnt). This function will compare two lists of integers (e.g., bytes) lexicographically.

```bluespec "leftNeighbor" +=
val smallerKeyNodes = t.values().filter(n => match n {
  | Leaf(l) => less_than(l.key_hash, k)
  | Internal(_) => false
})
```

If there is no leaf nodes with smaller `key_hash`, we will return `None`.

```bluespec "leftNeighbor" +=
if(smallerKeyNodes.empty()) None else
```
<!---
```bluespec "leftNeighbor" +=
  <<<leftNeighbor1>>>
```
--->
If there are some leafs in `smallerKeyNodes`, the algorithm will find leaf that is the closest to the leaf with a `key_hash` passed into the function.

```bluespec "leftNeighbor1" +=
val someLeaf = smallerKeyNodes.fold({key_hash: [], value_hash: []}, (s, x) =>
  match x {
    | Leaf(l) =>
          l
    | Internal(_) => s
  })
Some(smallerKeyNodes.fold( someLeaf, (s,x) =>
  match x {
    | Leaf(l) =>
        if (less_than(s.key_hash, l.key_hash))
          l
        else
          s
    | Internal(_) => s
  }
))
```

## Get right neighbor

Algorithm for getting right neighbor works in the same way as previously described `leftNeighbor` function. It emulates the following piece of Rust code.

```rust
let opts = new_read_options(Some(version), None, None);
let mode = IteratorMode::From(&key_hash, Direction::Forward);
let right = self
    .inner
    .db
    .iterator_cf_opt(&cf, opts, mode)
    .next()
    .map(|res| {
        let (_, key) = res?;
        let value = state_storage.read(&key).unwrap();
        generate_existence_proof(key.to_vec(), value)
    })
    .transpose()?;
```
<!---
```bluespec "definitions" +=

/// Return leaf with the smallest key_hash larger than k
```
--->
```bluespec "definitions" +=
pure def rightNeighbor(t: TreeMap, k: BitArray): Option[LeafNode] =
  val largerKeyNodes = t.values().filter(n => match n {
    | Leaf(l) => less_than(k, l.key_hash)
    | Internal(_) => false
  })
  if(largerKeyNodes.empty()) None else
    val someLeaf = largerKeyNodes.fold({key_hash: [], value_hash: []}, (s, x) =>
      match x {
        | Leaf(l) =>
              l
        | Internal(_) => s
        })
    Some(largerKeyNodes.fold(someLeaf, (s, x) =>
      match x {
        | Leaf(l) =>
            if (less_than(l.key_hash, s.key_hash) )
              l
            else
              s
        | Internal(_) => s
      }
    ))
```
<!---
```bluespec "definitions" +=

/// Returns optional CommitmentProof based for the given key_hash.
/// In implementation the key is passed instead of the key_hash.
```
--->
## Generating commitment proof

First, the signature:

```rust
fn ics23_prove(
    &self,
    key: Vec<u8>,
    version: Option<u64>,
) -> Result<CommitmentProof, Self::Error> {
```

In Quint, we are using `Option` to handle all possible errors that could occur in `ics23_prove`.

```bluespec "definitions" +=
pure def ics23_prove(t: Tree, key_hash: BitArray, version: Version): Option[CommitmentProof] =
```

We get a tree for the given `version`. This corresponds to the following Rust code.

```rust
let state_storage = self.state_storage(Some(version))?;
```

Then get all leaves in that `version`. Then we perform filtering, which will leave either the leaf which we are looking for, or an empty list. Then we map that leaf into `Some` of that leaf's `value_hash`.
<!---
```bluespec "definitions" +=
  <<<ics23_prove>>>
```
--->
```bluespec "ics23_prove" +=
val optionalValueForKey = t.treeAtVersion(version)
                           .allLeafs()
                           .filter(l => l.key_hash == key_hash)
                           .map(l => Some(l.value_hash))
```

Next we needed to emualte the following Rust code.

```rust
let proof = match state_storage.read(&key) {
  Some(value) => (...)
  None => (...)
}
```

To get as close as possible to the Rust implementation, we created `state_storage_read` variable.

```bluespec "ics23_prove" +=
val state_storage_read = if (optionalValueForKey.empty()) None else optionalValueForKey.getOnlyElement()
```
<!---
```bluespec "ics23_prove" +=
match state_storage_read {
  | Some(value) =>
    <<<existence>>>
  | None =>
    <<<nonexistence>>>
}
```
--->
Then we match `state_storage_read` variable.

```bluespec
match state_storage_read {
    | Some(value) =>
      // generating existence proof
      (...)
    | None =>
      // generating non-existence proof
      (...)
}
```

If the value is found, we will generate an ICS-23 existence proof, like it was done in Rust implementation:

```rust
let generate_existence_proof = |key: Vec<u8>, value| -> DbResult<_> {
    let key_hash = key.sha2_256();
    let path = MERKLE_TREE.ics23_prove_existence(&state_commitment, version, key_hash)?;

    Ok(ExistenceProof {
        key,
        value,
        leaf: ICS23_PROOF_SPEC.leaf_spec.clone(),
        path,
    })
};
let proof = match state_storage.read(&key) {
            // Value is found. Generate an ICS-23 existence proof.
            Some(value) => CommitmentProofInner::Exist(generate_existence_proof(key, value)?),
            None => (...)
}
```

Since Rust `ics23_prove_existence` will panic if there is no leaf with a given `key_hash`, we again used `None` as a way to panic `ics23_prove` if Quint `ics23_prove_existence` function panics.

```bluespec "existence" +=
val p = ics23_prove_existence(t, version, key_hash)
match p {
  | Some(path) =>
      Some(Exist(
        { key: key_hash,
          value: value,
          leaf: { prefix: LeafNodeHashPrefix },
          path: path }
      ))
  | None => None
}
```

However, if the algorithm did not find a leaf, it will end up in the `None` `match` branch and therefore generate `NonExistenceProof` proof. Since `NonExistenceProof` consists of `key` and optional ExistenceProofs for left and right neighbor, first we will look for left neighbor of a leaf for which we are generating `NonExistenceProof` at a given version.

```bluespec "nonexistence" +=
val lneighborOption: Option[LeafNode] = leftNeighbor(t.treeAtVersion(version), key_hash)
```

Then, the algorithm matches `lneighborOption`. If there is a left neighbor, it will call `ics23_prove_existence`, create a path to the left neighbor and create `Some` of `ExistenceProof`. The algorithm returns `None` if there is no left neighbor or path.

```bluespec "nonexistence" +=
val leftNeighborExistenceProof: Option[ExistenceProof] = match lneighborOption {
  | Some(lneighbor) =>
    val pathOption = ics23_prove_existence(t, version, lneighbor.key_hash)
    match pathOption {
      | Some(path) => Some({
        key: lneighbor.key_hash,
        value: lneighbor.value_hash,
        leaf: { prefix: LeafNodeHashPrefix },
        path: path
      })
      | None => None
    }
  | None => None
}
```

The algorithm will create ExistenceProof of a right neighbor in the same way it did for the left one.

```bluespec "nonexistence" +=
val rneighborOption: Option[LeafNode]  = rightNeighbor(t.treeAtVersion(version), key_hash)
val rightNeighborExistenceProof: Option[ExistenceProof] = match rneighborOption {
  | Some(rneighbor) =>
    val pathOption = ics23_prove_existence(t,version, rneighbor.key_hash)
    match pathOption {
      | Some(path) => Some({
        key: rneighbor.key_hash,
        value: rneighbor.value_hash,
        leaf: { prefix: LeafNodeHashPrefix },
        path: path
      })
      | None => None
    }
  | None => None
}
```

After that, the algorithm wraps those two `ExistenceProof`s together into `NonExistenceProof` and returns `Some` of that `NonExistenceProof`.

```bluespec "nonexistence" +=
val nep: NonExistenceProof = { key: key_hash,
                               left: leftNeighborExistenceProof,
                               right: rightNeighborExistenceProof }
Some(NonExist(nep))
```

Generation of `NonExistenceProof` emulates the following Rust code:

```rust
let cf = cf_preimages(&self.inner.db);
let key_hash = key.sha2_256();

let opts = new_read_options(Some(version), None, None);
let mode = IteratorMode::From(&key_hash, Direction::Reverse);
let left = self
    .inner
    .db
    .iterator_cf_opt(&cf, opts, mode)
    .next()
    .map(|res| {
        let (_, key) = res?;
        let value = state_storage.read(&key).unwrap();
        generate_existence_proof(key.to_vec(), value)
    })
    .transpose()?;

let opts = new_read_options(Some(version), None, None);
let mode = IteratorMode::From(&key_hash, Direction::Forward);
let right = self
    .inner
    .db
    .iterator_cf_opt(&cf, opts, mode)
    .next()
    .map(|res| {
        let (_, key) = res?;
        let value = state_storage.read(&key).unwrap();
        generate_existence_proof(key.to_vec(), value)
    })
    .transpose()?;

CommitmentProofInner::Nonexist(NonExistenceProof { key, left, right })
```
