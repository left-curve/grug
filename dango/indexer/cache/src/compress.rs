//! Compression layer for blocks and batches, independent of S3 transport.
//!
//! - [`BlockCompressor`] handles the per-block legacy-LZMA format (`.borsh.xz`).
//! - [`BatchCompressor`] packs many blocks into a tar and compresses it with a
//!   pluggable [`Codec`], and reads them back — eagerly (`decompress`) or lazily
//!   one block at a time (`reader` -> [`BatchReader`]).
//! - [`decode_block`] turns raw borsh bytes into the typed block.
//!
//! The [`Codec`] indirection is what makes this testable without external tools:
//! tests use [`Stored`] (no compression, no `xz` binary), while production uses a
//! real codec. [`Xz`] (behind the `xz-codec` feature) is an in-process liblzma
//! codec; a cross-compiled producer can instead implement `Codec` by shelling
//! out to `xz`.

use {
    crate::error::{IndexerError, Result},
    dango_primitives::BlockAndBlockOutcomeWithHttpDetails,
    lzma_rs::{lzma_compress, lzma_decompress},
    std::{
        io::{Read, Write},
        path::Path,
    },
    tar::{Archive, Builder, Header},
};

// --------------------------------- codec -----------------------------------

/// A byte-level compression codec (coder/decoder) for the batch outer layer.
pub trait Codec {
    /// Compress a buffer.
    fn compress(&self, data: &[u8]) -> Result<Vec<u8>>;

    /// Decompress a whole buffer at once.
    fn decompress(&self, data: &[u8]) -> Result<Vec<u8>>;

    /// Wrap a reader so its bytes are decompressed on demand — the basis for
    /// streaming a batch one block at a time without materializing the whole
    /// decompressed tar.
    fn decode_reader(&self, input: Box<dyn Read + Send>) -> Result<Box<dyn Read + Send>>;

    /// Wrap `out` in a streaming encoder for incremental compression. Block
    /// bytes written through it (via the tar builder) are compressed into a
    /// single stream — same ratio as compressing the whole tar at once. Call
    /// [`Encode::finish`] when done to flush the codec trailer.
    fn encode<W: Write + 'static>(&self, out: W) -> Box<dyn Encode<W>>;
}

/// A streaming compression sink produced by [`Codec::encode`]: a `Write` whose
/// input is compressed into the wrapped writer. Call [`Self::finish`] when done
/// to finalize the stream and recover the inner writer.
pub trait Encode<W>: Write {
    fn finish(self: Box<Self>) -> Result<W>;
}

/// No-op codec: data passes through uncompressed. For tests (no external tools)
/// and for composing when compression happens elsewhere.
pub struct Stored;

impl Codec for Stored {
    fn compress(&self, data: &[u8]) -> Result<Vec<u8>> {
        Ok(data.to_vec())
    }

    fn decompress(&self, data: &[u8]) -> Result<Vec<u8>> {
        Ok(data.to_vec())
    }

    fn decode_reader(&self, input: Box<dyn Read + Send>) -> Result<Box<dyn Read + Send>> {
        Ok(input)
    }

    fn encode<W: Write + 'static>(&self, out: W) -> Box<dyn Encode<W>> {
        Box::new(StoredEncode(out))
    }
}

/// Pass-through encoder for [`Stored`].
struct StoredEncode<W>(W);

impl<W: Write> Write for StoredEncode<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl<W: Write + 'static> Encode<W> for StoredEncode<W> {
    fn finish(self: Box<Self>) -> Result<W> {
        Ok(self.0)
    }
}

/// In-process xz codec (liblzma via `xz2`). Behind the `xz-codec` feature so the
/// base build stays pure-Rust.
#[cfg(feature = "xz-codec")]
pub struct Xz {
    pub level: u32,
}

#[cfg(feature = "xz-codec")]
impl Xz {
    pub fn new(level: u32) -> Self {
        Self { level }
    }
}

#[cfg(feature = "xz-codec")]
impl Default for Xz {
    fn default() -> Self {
        Self { level: 9 }
    }
}

#[cfg(feature = "xz-codec")]
impl Codec for Xz {
    fn compress(&self, data: &[u8]) -> Result<Vec<u8>> {
        let mut encoder = xz2::write::XzEncoder::new(Vec::new(), self.level);
        encoder.write_all(data)?;
        Ok(encoder.finish()?)
    }

    fn decompress(&self, data: &[u8]) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        xz2::read::XzDecoder::new(data).read_to_end(&mut out)?;
        Ok(out)
    }

    fn decode_reader(&self, input: Box<dyn Read + Send>) -> Result<Box<dyn Read + Send>> {
        Ok(Box::new(xz2::read::XzDecoder::new(input)))
    }

    fn encode<W: Write + 'static>(&self, out: W) -> Box<dyn Encode<W>> {
        Box::new(XzEncode(xz2::write::XzEncoder::new(out, self.level)))
    }
}

/// Streaming xz encoder for [`Xz`].
#[cfg(feature = "xz-codec")]
struct XzEncode<W: Write>(xz2::write::XzEncoder<W>);

#[cfg(feature = "xz-codec")]
impl<W: Write> Write for XzEncode<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

#[cfg(feature = "xz-codec")]
impl<W: Write + 'static> Encode<W> for XzEncode<W> {
    fn finish(self: Box<Self>) -> Result<W> {
        Ok(self.0.finish()?)
    }
}

// ----------------------------- single block --------------------------------

/// Compress/decompress a single block in the node's legacy-LZMA format.
///
/// The bot only ever *decompresses* stored blocks; `compress` mirrors the node
/// (lzma-rs) and exists for symmetry and round-trip tests.
pub struct BlockCompressor;

impl BlockCompressor {
    /// Stored block bytes (legacy LZMA1) -> raw borsh.
    pub fn decompress(&self, stored: &[u8]) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        lzma_decompress(&mut &stored[..], &mut out)
            .map_err(|e| IndexerError::byte_stream(e.to_string()))?;
        Ok(out)
    }

    /// Raw borsh -> legacy LZMA1 (matches the node's format).
    pub fn compress(&self, raw: &[u8]) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        lzma_compress(&mut &raw[..], &mut out)
            .map_err(|e| IndexerError::byte_stream(e.to_string()))?;
        Ok(out)
    }
}

// -------------------------------- batch ------------------------------------

/// Pack blocks into a tar of `<height>.borsh` entries and compress with `C`.
pub struct BatchCompressor<C> {
    codec: C,
}

impl<C> BatchCompressor<C>
where
    C: Codec,
{
    pub fn new(codec: C) -> Self {
        Self { codec }
    }

    /// Build the tar from `(height, raw borsh)` pairs and compress it. Buffers
    /// the whole tar in memory — use [`Self::compress_to`] for large batches.
    pub fn compress(&self, blocks: &[(u64, Vec<u8>)]) -> Result<Vec<u8>> {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = Builder::new(&mut tar_bytes);
            for (height, raw) in blocks {
                append_block(&mut builder, *height, raw)?;
            }
            builder.finish()?;
        }
        self.codec.compress(&tar_bytes)
    }

    /// Stream blocks from an iterator into a compressed archive written to
    /// `out`, one block at a time. Peak memory is ~one block (plus the codec's
    /// working buffers), and the ratio is identical to [`Self::compress`] — the
    /// blocks still go through a single compression stream. Returns the finished
    /// `out` writer.
    pub fn compress_to<W, I>(&self, out: W, blocks: I) -> Result<W>
    where
        W: Write + 'static,
        I: IntoIterator<Item = Result<(u64, Vec<u8>)>>,
    {
        let mut builder = Builder::new(self.codec.encode(out));
        for block in blocks {
            let (height, raw) = block?;
            append_block(&mut builder, height, &raw)?;
        }
        // `into_inner` writes the tar trailer (through the encoder), then
        // `finish` flushes the codec's own trailer and returns `out`.
        builder.into_inner()?.finish()
    }

    /// Eagerly decompress the whole archive into `(height, raw borsh)` pairs.
    /// Holds everything in memory — use [`Self::reader`] for large batches.
    pub fn decompress(&self, archive: &[u8]) -> Result<Vec<(u64, Vec<u8>)>> {
        let tar_bytes = self.codec.decompress(archive)?;
        let mut archive = Archive::new(&tar_bytes[..]);
        let mut out = Vec::new();
        for entry in archive.entries()? {
            let mut entry = entry?;
            let height = height_from_path(entry.path()?.as_ref())?;
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            out.push((height, buf));
        }
        Ok(out)
    }

    /// Open the (compressed) archive for streaming iteration. The caller owns
    /// the returned reader; decompression happens lazily as blocks are pulled.
    ///
    /// `input` is any `Read + Send + 'static` source, so the archive can come
    /// from memory or straight off disk:
    ///
    /// ```ignore
    /// // In-memory bytes (e.g. an object freshly downloaded from S3):
    /// let mut reader = compressor.reader(std::io::Cursor::new(bytes))?;
    ///
    /// // Or stream from a file on disk without loading it all into memory:
    /// let file = std::fs::File::open("0-1000.tar.xz")?;
    /// let mut reader = compressor.reader(file)?;
    /// ```
    pub fn reader<R>(&self, input: R) -> Result<BatchReader>
    where
        R: Read + Send + 'static,
    {
        let decoded = self.codec.decode_reader(Box::new(input))?;
        Ok(BatchReader {
            archive: Archive::new(decoded),
        })
    }
}

/// Streaming reader over a batch archive. Iterating decompresses and yields one
/// block at a time (sequential — a solid archive can't be read out of order).
pub struct BatchReader {
    archive: Archive<Box<dyn Read + Send>>,
}

impl BatchReader {
    /// Lazily yield `(height, raw borsh)`, decompressing each entry on demand.
    pub fn blocks(&mut self) -> Result<impl Iterator<Item = Result<(u64, Vec<u8>)>> + '_> {
        let entries = self.archive.entries()?;
        Ok(entries.map(|entry| {
            let mut entry = entry?;
            let height = height_from_path(entry.path()?.as_ref())?;
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            Ok((height, buf))
        }))
    }

    /// Lazily yield decoded blocks.
    pub fn decoded(
        &mut self,
    ) -> Result<impl Iterator<Item = Result<(u64, BlockAndBlockOutcomeWithHttpDetails)>> + '_> {
        Ok(self.blocks()?.map(|item| {
            let (height, raw) = item?;
            Ok((height, decode_block(&raw)?))
        }))
    }
}

// ------------------------------- helpers -----------------------------------

/// Append one block to a tar builder as a `<height>.borsh` entry.
fn append_block<W: Write>(builder: &mut Builder<W>, height: u64, raw: &[u8]) -> Result<()> {
    let mut header = Header::new_gnu();
    header.set_size(raw.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0); // deterministic archive
    header.set_cksum();
    builder.append_data(&mut header, format!("{height}.borsh"), raw)?;
    Ok(())
}

/// Deserialize raw borsh bytes into the typed block.
pub fn decode_block(raw: &[u8]) -> Result<BlockAndBlockOutcomeWithHttpDetails> {
    Ok(borsh::from_slice(raw)?)
}

/// Parse the block height out of a `<height>.borsh` tar entry name.
fn height_from_path(path: &Path) -> Result<u64> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| IndexerError::byte_stream("batch entry has no filename".to_string()))?;
    let height = name.strip_suffix(".borsh").unwrap_or(name);
    Ok(height.parse::<u64>()?)
}

// -------------------------------- tests ------------------------------------

#[cfg(test)]
mod tests {
    use {
        super::{BatchCompressor, BlockCompressor, Stored},
        crate::error::Result,
        dango_primitives::{
            Block, BlockAndBlockOutcomeWithHttpDetails, BlockInfo, BlockOutcome, Hash256, Timestamp,
        },
        std::{collections::HashMap, io::Cursor},
    };

    fn sample() -> Vec<(u64, Vec<u8>)> {
        vec![
            (1, b"alpha".to_vec()),
            (2, b"beta".to_vec()),
            (10, b"gamma gamma gamma".to_vec()),
        ]
    }

    /// A minimal, deterministic block keyed by `height` — empty txs/outcomes,
    /// zero hashes — enough to exercise the borsh round-trip through the reader.
    fn mock_block(height: u64) -> BlockAndBlockOutcomeWithHttpDetails {
        BlockAndBlockOutcomeWithHttpDetails {
            block: Block {
                info: BlockInfo {
                    height,
                    timestamp: Timestamp::from_seconds(height as u128),
                    hash: Hash256::ZERO,
                },
                txs: vec![],
            },
            block_outcome: BlockOutcome {
                height,
                app_hash: Hash256::ZERO,
                cron_outcomes: vec![],
                tx_outcomes: vec![],
            },
            http_request_details: HashMap::new(),
        }
    }

    #[test]
    fn batch_bytes_roundtrip_stored() {
        let c = BatchCompressor::new(Stored);
        let archive = c.compress(&sample()).unwrap();
        assert_eq!(c.decompress(&archive).unwrap(), sample());
    }

    #[test]
    fn batch_streaming_reader_stored() {
        let c = BatchCompressor::new(Stored);
        let archive = c.compress(&sample()).unwrap();
        let mut reader = c.reader(Cursor::new(archive)).unwrap();
        let got = reader
            .blocks()
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(got, sample());
    }

    #[test]
    fn batch_streaming_compress_stored() {
        let c = BatchCompressor::new(Stored);
        // Feed blocks lazily from an iterator; get the finished archive back.
        let archive = c
            .compress_to(Vec::new(), sample().into_iter().map(Ok))
            .unwrap();

        // Streaming compress must produce the same archive as eager compress.
        assert_eq!(archive, c.compress(&sample()).unwrap());

        // And it round-trips.
        let mut reader = c.reader(Cursor::new(archive)).unwrap();
        let got = reader
            .blocks()
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(got, sample());
    }

    #[test]
    fn batch_reader_decoded_roundtrip_stored() {
        let blocks = [mock_block(1), mock_block(2), mock_block(10)];

        // Pack the borsh-encoded blocks into a batch...
        let raw = blocks
            .iter()
            .map(|b| (b.block.info.height, borsh::to_vec(b).unwrap()))
            .collect::<Vec<_>>();
        let c = BatchCompressor::new(Stored);
        let archive = c.compress(&raw).unwrap();

        // ...then read them back through the decoding path, which deserializes
        // each entry into a typed block on demand.
        let mut reader = c.reader(Cursor::new(archive)).unwrap();
        let got = reader
            .decoded()
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();

        let expected = blocks
            .iter()
            .map(|b| (b.block.info.height, b.clone()))
            .collect::<Vec<_>>();
        assert_eq!(got, expected);
    }

    #[test]
    fn block_lzma_roundtrip() {
        let bc = BlockCompressor;
        let raw = b"the quick brown fox jumps over the lazy dog".to_vec();
        let compressed = bc.compress(&raw).unwrap();
        assert_eq!(bc.decompress(&compressed).unwrap(), raw);
    }

    #[cfg(feature = "xz-codec")]
    #[test]
    fn batch_roundtrip_xz() {
        use super::Xz;

        let c = BatchCompressor::new(Xz::default());
        let archive = c.compress(&sample()).unwrap();
        assert_eq!(c.decompress(&archive).unwrap(), sample());

        let mut reader = c.reader(Cursor::new(archive)).unwrap();
        let got = reader
            .blocks()
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(got, sample());

        // Streaming compress with the real codec round-trips too.
        let streamed = c
            .compress_to(Vec::new(), sample().into_iter().map(Ok))
            .unwrap();
        assert_eq!(c.decompress(&streamed).unwrap(), sample());
    }
}
