# Indexed map

An `IndexedMap` is a map where each record is indexed not only by the primary key, but also by one or more other indexes.

For example, consider limit orders in an oracle-based perpetual futures protocol. For simplicity, let's just think about _buy_ orders:

```rust
struct Order {
    pub trader: Addr,
    pub limit_price: Udec256,
    pub expiration: Timestamp,
}
```

For each order, we generate a unique `OrderId`, which can be an incrementing number, and store orders in a map indexed by the IDs:

```rust
const ORDERS: Map<OrderId, Order> = Map::new("order");
```

During the block, users submit orders. Then, at the end of the block (utilizing the `after_block` function), a contract is called to do two things:

- Find all buy orders with limit prices below the oracle price; execute these orders.
- Find all orders with expiration time earlier than the current block time; delete these orders.

To achieve this, the orders need to be indexed by not only the order IDs, but also their limit prices and expiration times.

For this, we can convert `Orders` to the following `IndexedMap`:

```rust
#[index_list]
struct OrderIndexes<'a> {
    pub limit_price: MultiIndexMap<'a, OrderId, Udec256, Order>,
    pub expiration: MultiIndexMap<'a, OrderId, Timestamp, Order>,
}

const ORDERS: IndexedMap<OrderId, Order, OrderIndexes> = IndexedMap::new("orders", OrderIndexes {
    limit_price: MultiIndexMap::new(
        |order| *order.limit_price,
        "owners",
        "orders__price",
    ),
    expiration: MultiIndexMap::new(
        |order| *order.expiration,
        "owners",
        "orders__exp",
    ),
});
```

Here we use `MultiIndexMap`, which is an index type where multiple records in the map can have the same index. This is the appropriate choice here, since surely it's possible that two orders have the same limit price or expiration.

However, in cases where indexes are supposed to be unique (no two records shall have the same index), `UniqueIndexMap` can be used. It will throw an error if you attempt to save two records with the same index.

To find all orders whose limit prices are below the oracle price:

```rust
fn find_fillable_orders(
    storage: &dyn Storage,
    oracle_price: Udec256.
) -> StdResult<Vec<(OrderId, Order)>> {
    ORDERS
        .idx
        .limit_price
        .range(storage, None, Some(oracle_price), Order::Ascending)
        .map(|item| {
            // This iterator includes the limit price, which we don't need.
            let (_limit_price, order_id, order) = item?;
            Ok((order_id, order))
        })
        .collect()
}
```

Similarly, find and purge all orders whose expiration is before the current block time:

```rust
fn purge_expired_orders(
    storage: &mut dyn Storage,
    block_time: Timestamp,
) -> StdResult<()> {
    // We need to first collect order IDs into a vector, because the iteration
    // holds an immutable reference to `storage`, while the removal operations
    // require a mutable reference to it, which can't exist at the same time.
    let order_ids = ORDERS
        .index
        .expiration
        .range(storage, None, Some(block_time), Order::Ascending)
        .map(|item| {
            let (_, order_id, _) = item?;
            Ok(order_id)
        })
        .collect::<StdResult<Vec<OrderId>>>()?;

    for order_id in order_ids {
        ORDERS.remove(storage, order_id);
    }

    Ok(())
}
```
