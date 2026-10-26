//! Incremental decoding with bounded input/output buffers.
use crate::storage::StoreError;
use std::io::{self, Write};

/// Maximum compressed bytes accepted in one feed call.
pub const MAX_CHUNK: usize = 256 * 1024;
const STAGING_LIMIT: usize = 32 * 1024 * 1024;

/// A writer that refuses to exceed the decoded staging budget.
#[derive(Default)]
pub struct StagingBuffer {
    bytes: Vec<u8>,
}
impl Write for StagingBuffer {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if self.bytes.len().saturating_add(input.len()) > STAGING_LIMIT {
            return Err(io::Error::other(
                "Compressed input expands beyond 32 MiB before yielding",
            ));
        }
        self.bytes.extend_from_slice(input);
        Ok(input.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Decoded input format.
pub enum Decoder {
    /// Uncompressed UTF-8.
    Plain,
    /// Gzip, including concatenated members.
    Gzip(flate2::write::MultiGzDecoder<StagingBuffer>),
    /// Zstandard frame stream.
    Zstd {
        /// Frame decoder.
        decoder: Box<ruzstd::decoding::FrameDecoder>,
        /// Buffered compressed input.
        input: Vec<u8>,
        /// Whether a frame header was accepted.
        initialized: bool,
    },
}
impl Decoder {
    /// Chooses a decoder from the file name.
    pub fn from_name(name: &str) -> Self {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".gz") {
            Self::Gzip(flate2::write::MultiGzDecoder::new(StagingBuffer::default()))
        } else if lower.ends_with(".zst") || lower.ends_with(".zstd") {
            Self::Zstd {
                decoder: Box::new(ruzstd::decoding::FrameDecoder::new()),
                input: Vec::new(),
                initialized: false,
            }
        } else {
            Self::Plain
        }
    }
    /// Decodes one bounded chunk into slices capped at 32 MiB total.
    pub fn feed(&mut self, data: &[u8]) -> Result<Vec<Vec<u8>>, StoreError> {
        if data.len() > MAX_CHUNK {
            return Err(StoreError::Input("Input chunk exceeds 256 KiB".into()));
        }
        match self {
            Self::Plain => Ok(vec![data.to_vec()]),
            Self::Gzip(decoder) => {
                decoder
                    .write_all(data)
                    .map_err(|e| StoreError::Input(format!("Invalid gzip stream: {e}")))?;
                let output = std::mem::take(&mut decoder.get_mut().bytes);
                Ok(output
                    .chunks(64 * 1024)
                    .map(|chunk| chunk.to_vec())
                    .collect())
            }
            Self::Zstd {
                decoder,
                input,
                initialized,
            } => {
                input.extend_from_slice(data);
                let mut output = vec![0u8; 64 * 1024];
                let mut chunks = Vec::new();
                let mut total = 0;
                loop {
                    if !*initialized && input.len() < 18 {
                        break;
                    }
                    if *initialized && decoder.is_finished() && decoder.can_collect() == 0 {
                        if input.len() < 18 {
                            break;
                        }
                        **decoder = ruzstd::decoding::FrameDecoder::new();
                        *initialized = false;
                    }
                    if *initialized && !decoder.is_finished() && input.len() < 4 {
                        break;
                    }
                    let (read, written) = decoder
                        .decode_from_to(input, &mut output)
                        .map_err(|e| StoreError::Input(format!("Invalid Zstandard stream: {e}")))?;
                    *initialized = true;
                    if read > 0 {
                        input.drain(..read);
                    }
                    if written > 0 {
                        total += written;
                        chunks.push(output[..written].to_vec());
                    }
                    if total > STAGING_LIMIT {
                        return Err(StoreError::Input(
                            "Compressed chunk exceeds the 32 MiB staging budget".into(),
                        ));
                    }
                    if read == 0 && written == 0 {
                        break;
                    }
                }
                if input.len() > MAX_CHUNK * 2 {
                    return Err(StoreError::Input(
                        "Zstandard decoder made no progress".into(),
                    ));
                }
                Ok(chunks)
            }
        }
    }
    /// Validates the end of the stream and returns trailing decoded bytes.
    pub fn finish(&mut self) -> Result<Vec<u8>, StoreError> {
        match self {
            Self::Plain => Ok(Vec::new()),
            Self::Gzip(decoder) => {
                decoder
                    .try_finish()
                    .map_err(|e| StoreError::Input(format!("Incomplete gzip stream: {e}")))?;
                Ok(std::mem::take(&mut decoder.get_mut().bytes))
            }
            Self::Zstd {
                decoder,
                input,
                initialized,
            } => {
                let mut trailing = Vec::new();
                if !input.is_empty() && decoder.is_finished() {
                    **decoder = ruzstd::decoding::FrameDecoder::new();
                    *initialized = false;
                }
                if !input.is_empty() && !*initialized {
                    let mut output = vec![0u8; 64 * 1024];
                    let (read, written) = decoder
                        .decode_from_to(input, &mut output)
                        .map_err(|e| StoreError::Input(format!("Invalid Zstandard stream: {e}")))?;
                    input.drain(..read);
                    trailing.extend_from_slice(&output[..written]);
                    *initialized = true;
                    while decoder.can_collect() > 0 {
                        let (_, written) =
                            decoder.decode_from_to(&[], &mut output).map_err(|e| {
                                StoreError::Input(format!("Invalid Zstandard stream: {e}"))
                            })?;
                        if written == 0 {
                            break;
                        }
                        trailing.extend_from_slice(&output[..written]);
                        if trailing.len() > STAGING_LIMIT {
                            return Err(StoreError::Input(
                                "Zstandard output exceeds the 32 MiB staging budget".into(),
                            ));
                        }
                    }
                }
                if !decoder.is_finished() || !input.is_empty() {
                    return Err(StoreError::Input(format!(
                        "Incomplete Zstandard stream (frame finished: {}, trailing bytes: {})",
                        decoder.is_finished(),
                        input.len()
                    )));
                }
                Ok(trailing)
            }
        }
    }
}
