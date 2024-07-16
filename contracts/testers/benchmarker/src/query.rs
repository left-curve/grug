use grug::{Bound, Empty, Number, Order, StdResult, Storage, Uint128};

use crate::state::DATA;

// Function needs to be named `do_loop` instead of `loop`, because the latter is
// a reserved Rust keyword.
pub fn do_loop(iterations: u64) -> StdResult<Empty> {
    // Keep the same operation per iteration for consistency
    for _ in 0..iterations {
        let number = Uint128::new(100);
        number.checked_add(number)?;
        number.checked_sub(number)?;
        number.checked_mul(number)?;
        number.checked_div(number)?;
        number.checked_pow(2)?;
    }

    Ok(Empty {})
}

pub fn read_data(
    storage: &dyn Storage,
    min: Option<String>,
    max: Option<String>,
    order: Order,
    limit: u32,
    sized: bool,
) -> StdResult<Vec<(String, Uint128)>> {
    let min = min.as_ref().map(|val| Bound::exclusive(val.as_str()));
    let max = max.as_ref().map(|val| Bound::exclusive(val.as_str()));

    if sized {
        DATA.range_sized(storage, min, max, order, limit).collect()
    } else {
        DATA.range(storage, min, max, order)
            .take(limit as usize)
            .collect()
    }
}
