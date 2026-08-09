//! Interactive walk through batch archives stored in S3/B2.
//!
//! Downloads `<start>-<end>.tar.xz` archives from the `v1/` prefix of the
//! `dango-testnet-batches` bucket and streams them through
//! [`BatchCompressor::reader`], decompressing one block at a time:
//!
//! - press **enter** to decompress the next block;
//! - type anything else (then enter) to download the next archive.
//!
//! Credentials come from the environment (a `.env` in the working directory
//! or any parent is loaded automatically):
//!
//! ```text
//! S3_ENDPOINT=https://s3.us-east-005.backblazeb2.com
//! S3_ACCESS_KEY=...
//! S3_SECRET_KEY=...
//! S3_BUCKET=dango-testnet-batches   # optional, this is the default
//! S3_REGION=us-east-005             # optional, defaults to us-east-1
//! ```
//!
//! Run with:
//!
//! ```text
//! cargo run -p dango-indexer-cache --example batch_reader --features s3,xz-codec
//! ```

use {
    dango_indexer_cache::{
        BatchCompressor, Xz,
        s3::{Client, S3Config},
    },
    std::io::{Cursor, Write},
};

/// Number of blocks per batch archive (must match the producer).
const BATCH_SIZE: u64 = 10_000;

fn required_env(key: &str) -> anyhow::Result<String> {
    std::env::var(key).map_err(|_| anyhow::anyhow!("missing `{key}` in environment / .env"))
}

fn optional_env(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load `.env` if present; real environment variables take precedence.
    dotenvy::dotenv().ok();

    let cfg = S3Config {
        enabled: true,
        path: String::new(),
        endpoint: required_env("S3_ENDPOINT")?,
        access_key: required_env("S3_ACCESS_KEY")?,
        secret_key: required_env("S3_SECRET_KEY")?,
        bucket: optional_env("S3_BUCKET", "dango-testnet-batches"),
        region: optional_env("S3_REGION", "us-east-1"),
    };
    let bucket = cfg.bucket.clone();

    let client = Client::new(cfg).await?;
    let compressor = BatchCompressor::new(Xz::default());

    // Archives are named `1-9999.tar.xz`, `10000-19999.tar.xz`, ... — the
    // first range starts at 1, every range ends on a multiple-of-10000 - 1.
    let (mut start, mut end) = (1, BATCH_SIZE - 1);

    'archives: loop {
        let key = format!("v1/{start}-{end}.tar.xz");
        println!("downloading s3://{bucket}/{key} ...");

        let Some(bytes) = client.get(&key).await? else {
            println!("{key} not found in bucket, stopping");
            break;
        };
        println!("downloaded {} bytes", bytes.len());

        // Lazy reader: each block is decompressed only when pulled off the
        // iterator, so pausing between blocks costs nothing.
        let mut reader = compressor.reader(Cursor::new(bytes))?;
        let mut blocks = reader.decoded()?;

        loop {
            let Some(item) = blocks.next() else {
                println!("archive {key} exhausted, moving to the next one");
                break;
            };
            let (height, block) = item?;
            println!(
                "block {height}: hash {}, {} txs, timestamp {:?}",
                block.block.info.hash,
                block.block.txs.len(),
                block.block.info.timestamp,
            );

            print!("[enter] next block | [anything else] next archive > ");
            std::io::stdout().flush()?;

            let mut line = String::new();
            if std::io::stdin().read_line(&mut line)? == 0 {
                // stdin closed (EOF) — quit.
                break 'archives;
            }
            if !line.trim().is_empty() {
                break;
            }
        }

        (start, end) = (end + 1, end + BATCH_SIZE);
    }

    Ok(())
}
