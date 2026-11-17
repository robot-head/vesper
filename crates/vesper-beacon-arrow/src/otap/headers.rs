//! HPACK (RFC 7541) decoding of `BatchArrowRecords::headers`.
//!
//! Upstream's exporter puts its outgoing gRPC metadata (W3C trace context,
//! auth or tenant headers) in each batch's `headers`, encoded by one
//! `hpack.Encoder` per stream, so the dynamic table carries over from batch
//! to batch and a stream's header blocks must be decoded in order with one
//! decoder. [`HeaderDecoder`] is that decoder: `loona-hpack` (the
//! maintained fork of the `hpack` crate) behind Beacon's own bounds.
//!
//! Bounds: the dynamic table may grow to at most `max_table_size` bytes
//! (a larger size update is an error), and the decoded fields of one block
//! (name + value + 32 bytes each, the RFC 7541 entry size) may total at most
//! `max_decoded_bytes`; fields past that are not copied. Field names must
//! be UTF-8. Any failure, including a panic inside the HPACK decoder, is a
//! typed error and leaves the decoder unusable for the stream (its table
//! may be out of step with the encoder's), so the caller resets it.

use super::decode::DecodeError;

/// Default dynamic table limit (the HTTP/2 default the Go encoder uses).
pub const DEFAULT_HPACK_TABLE_BYTES: usize = 4096;

/// One decoded header field: a name and its raw value bytes (values of
/// `-bin` keys are binary).
pub type HeaderField = (String, Vec<u8>);

/// A stream's HPACK decoder.
pub struct HeaderDecoder {
    inner: loona_hpack::Decoder<'static>,
    max_decoded_bytes: usize,
}
impl std::fmt::Debug for HeaderDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeaderDecoder")
            .field("max_decoded_bytes", &self.max_decoded_bytes)
            .finish_non_exhaustive()
    }
}
impl HeaderDecoder {
    pub fn new(max_table_size: usize, max_decoded_bytes: usize) -> Self {
        let mut inner = loona_hpack::Decoder::new();
        inner.set_max_allowed_table_size(max_table_size);
        // The initial table size is the protocol default (4096); a smaller
        // allowance takes effect immediately.
        if max_table_size < DEFAULT_HPACK_TABLE_BYTES {
            inner.set_max_table_size(max_table_size);
        }
        Self {
            inner,
            max_decoded_bytes,
        }
    }
    /// Decodes one header block.
    pub fn decode(&mut self, block: &[u8]) -> Result<Vec<HeaderField>, DecodeError> {
        let max = self.max_decoded_bytes;
        let inner = &mut self.inner;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut fields = Vec::new();
            let mut used = 0usize;
            let mut over = false;
            let mut bad_name = false;
            inner
                .decode_with_cb(block, |name, value| {
                    if over || bad_name {
                        return;
                    }
                    used = used
                        .saturating_add(name.len())
                        .saturating_add(value.len())
                        .saturating_add(32);
                    if used > max {
                        over = true;
                        return;
                    }
                    match String::from_utf8(name.into_owned()) {
                        Ok(name) => fields.push((name, value.into_owned())),
                        Err(_) => bad_name = true,
                    }
                })
                .map_err(|e| DecodeError::BadHeaders(hpack_error(&e)))?;
            if over {
                return Err(DecodeError::HeadersTooLarge);
            }
            if bad_name {
                return Err(DecodeError::BadHeaders("header name is not UTF-8"));
            }
            Ok(fields)
        }))
        .map_err(|_| DecodeError::BadHeaders("HPACK decoder panicked"))?
    }
}

fn hpack_error(e: &loona_hpack::decoder::DecoderError) -> &'static str {
    use loona_hpack::decoder::DecoderError as E;
    match e {
        E::HeaderIndexOutOfBounds => "header index out of bounds",
        E::IntegerDecodingError(_) => "bad HPACK integer",
        E::StringDecodingError(_) => "bad HPACK string",
        E::InvalidMaxDynamicSize => "dynamic table size update above the limit",
        E::SizeUpdateAtEnd => "size update at the end of a block",
        #[allow(unreachable_patterns)]
        _ => "bad HPACK block",
    }
}
