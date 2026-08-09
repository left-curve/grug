//! Fetch the entire `order_filled` history for a single user from the
//! Dango indexer and write it to a CSV file.
//!
//! Uses paginated GraphQL queries because an active account can have
//! thousands of fills.
//!
//! Run with:
//!
//! ```sh
//! cargo run -p dango-sdk --example trade_history_csv
//! ```

use {
    anyhow::{Context, Result},
    dango_primitives::EventName,
    dango_sdk::{HttpClient, PageInfo, perps_events},
    dango_types::perps::OrderFilled,
    serde::Serialize,
};

const HTTP_URL: &str = "https://api-mainnet.dango.zone";
const USER_ADDR: &str = "0x0000000000000000000000000000000000000000"; // replace with the actual address of interest
const OUTPUT_PATH: &str = "trades.csv";
// The indexer caps `first` at 100.
const PAGE_SIZE: i64 = 100;

#[derive(Debug, Serialize)]
struct CsvRow {
    block_height: i64,
    created_at: String,
    tx_hash: String,
    pair_id: String,
    user_addr: String,
    idx: i64,
    order_id: String,
    fill_id: Option<String>,
    is_maker: Option<bool>,
    fill_price: String,
    fill_size: String,
    closing_size: String,
    opening_size: String,
    realized_pnl: String,
    realized_funding: Option<String>,
    fee: String,
    client_order_id: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let client = HttpClient::new(HTTP_URL)?;

    println!("fetching order_filled events for {USER_ADDR} from {HTTP_URL}");

    let events = client
        .paginate_all(
            Some(PAGE_SIZE),
            None,
            |after, before, first, last| perps_events::Variables {
                after,
                before,
                first,
                last,
                user_addr: Some(USER_ADDR.to_string()),
                event_type: Some(OrderFilled::EVENT_NAME.to_string()),
                ..Default::default()
            },
            |data| {
                let pi = data.perps_events.page_info;
                (
                    data.perps_events.nodes,
                    PageInfo {
                        start_cursor: pi.start_cursor,
                        end_cursor: pi.end_cursor,
                        has_next_page: pi.has_next_page,
                        has_previous_page: pi.has_previous_page,
                    },
                )
            },
        )
        .await?;

    println!("fetched {} fills; writing to {OUTPUT_PATH}", events.len());

    let mut writer = csv::Writer::from_path(OUTPUT_PATH)
        .with_context(|| format!("opening {OUTPUT_PATH} for writing"))?;

    for ev in events {
        let parsed: OrderFilled = serde_json::from_value(ev.data.clone())
            .with_context(|| format!("decoding order_filled data at idx={}", ev.idx))?;

        writer.serialize(CsvRow {
            block_height: ev.block_height,
            created_at: ev.created_at,
            tx_hash: ev.tx_hash,
            pair_id: ev.pair_id,
            user_addr: ev.user_addr,
            idx: ev.idx,
            order_id: parsed.order_id.to_string(),
            fill_id: parsed.fill_id.map(|v| v.to_string()),
            is_maker: parsed.is_maker,
            fill_price: parsed.fill_price.to_string(),
            fill_size: parsed.fill_size.to_string(),
            closing_size: parsed.closing_size.to_string(),
            opening_size: parsed.opening_size.to_string(),
            realized_pnl: parsed.realized_pnl.to_string(),
            realized_funding: parsed.realized_funding.map(|v| v.to_string()),
            fee: parsed.fee.to_string(),
            client_order_id: parsed.client_order_id.map(|v| v.to_string()),
        })?;
    }

    writer.flush()?;

    println!("done; wrote {OUTPUT_PATH}");

    Ok(())
}
