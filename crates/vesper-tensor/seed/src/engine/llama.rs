//! The frozen Rust llama2.c readers (bug fixes only until self-hosting
//! Stage 2): `Config::validate`, `Model::from_llama2c` and
//! `Tokenizer::from_llama2c`. The package vesper-tensor runs the Vesper port
//! (vesper/src/llama.ves); this seed copy, which the compiler links, keeps the
//! Rust readers. Moved here from src/llama.rs by SH6 lane 11 (#1406).
use crate::llama::{
    Config, Layer, Model, Tokenizer, MAX_CHECKPOINT_BYTES, MAX_DIM, MAX_LAYERS, MAX_SEQ_LEN,
    MAX_TOKEN_LENGTH, MAX_VOCAB,
};
use crate::quant::Matrix;
use crate::Error;

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Reader<'a> {
    fn i32(&mut self) -> Result<i32, Error> {
        let b = self.take(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn f32(&mut self) -> Result<f32, Error> {
        let b = self.take(4)?;
        Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.offset.checked_add(n).ok_or(Error::Overflow)?;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or(Error::Malformed("truncated input"))?;
        self.offset = end;
        Ok(slice)
    }
    fn floats(&mut self, n: usize) -> Result<Vec<f32>, Error> {
        let bytes = self.take(n.checked_mul(4).ok_or(Error::Overflow)?)?;
        let mut out = Vec::new();
        out.try_reserve_exact(n)
            .map_err(|_| Error::Limit("allocation"))?;
        for b in bytes.as_chunks::<4>().0.iter() {
            let v = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            if !v.is_finite() {
                return Err(Error::NonFinite);
            }
            out.push(v);
        }
        Ok(out)
    }
    fn matrix(&mut self, rows: usize, cols: usize) -> Result<Matrix, Error> {
        let n = rows.checked_mul(cols).ok_or(Error::Overflow)?;
        Matrix::f32(rows, cols, self.floats(n)?)
    }
}

fn positive(v: i32, what: &'static str, max: usize) -> Result<usize, Error> {
    if v <= 0 {
        return Err(Error::Malformed(what));
    }
    let v = v as usize;
    if v > max {
        return Err(Error::Limit(what));
    }
    Ok(v)
}

/// Checks every dimension is positive and within the hard ceilings,
/// `dim % heads == 0`, `heads % kv_heads == 0` and an even head width.
pub(crate) fn validate(c: &Config) -> Result<(), Error> {
    let bounds = [
        (c.dim, MAX_DIM, "dim"),
        (c.hidden_dim, 4 * MAX_DIM, "hidden_dim"),
        (c.layers, MAX_LAYERS, "layers"),
        (c.heads, MAX_DIM, "heads"),
        (c.kv_heads, MAX_DIM, "kv_heads"),
        (c.vocab, MAX_VOCAB, "vocab"),
        (c.seq_len, MAX_SEQ_LEN, "seq_len"),
    ];
    for (value, max, what) in bounds {
        if value == 0 {
            return Err(Error::Malformed(what));
        }
        if value > max {
            return Err(Error::Limit(what));
        }
    }
    if !c.dim.is_multiple_of(c.heads) {
        return Err(Error::Shape("dim must be a multiple of heads"));
    }
    crate::shape::gqa_ratio(c.heads as u64, c.kv_heads as u64)?;
    if !c.head_dim().is_multiple_of(2) {
        return Err(Error::Shape("RoPE needs an even head width"));
    }
    if !(c.rope_base.is_finite() && c.rope_base > 1.0) {
        return Err(Error::Shape("rope base"));
    }
    if !(c.rms_epsilon.is_finite() && c.rms_epsilon > 0.0) {
        return Err(Error::InvalidEpsilon);
    }
    Ok(())
}

pub(crate) fn model(bytes: &[u8]) -> Result<Model, Error> {
    if bytes.len() > MAX_CHECKPOINT_BYTES {
        return Err(Error::Limit("checkpoint bytes"));
    }
    let mut r = Reader { bytes, offset: 0 };
    let dim = positive(r.i32()?, "dim", MAX_DIM)?;
    let hidden_dim = positive(r.i32()?, "hidden_dim", 4 * MAX_DIM)?;
    let layers = positive(r.i32()?, "layers", MAX_LAYERS)?;
    let heads = positive(r.i32()?, "heads", MAX_DIM)?;
    let kv_heads = positive(r.i32()?, "kv_heads", MAX_DIM)?;
    let raw_vocab = r.i32()?;
    let shared_classifier = raw_vocab > 0;
    let vocab = positive(
        raw_vocab.checked_abs().ok_or(Error::Malformed("vocab"))?,
        "vocab",
        MAX_VOCAB,
    )?;
    let seq_len = positive(r.i32()?, "seq_len", MAX_SEQ_LEN)?;
    let config = Config::llama2c(
        dim,
        hidden_dim,
        layers,
        heads,
        kv_heads,
        vocab,
        seq_len,
        shared_classifier,
    );
    config.validate()?;
    let head_dim = config.head_dim();
    let kv_dim = config.kv_dim();
    let embedding = r.floats(vocab.checked_mul(dim).ok_or(Error::Overflow)?)?;
    let per_layer = |r: &mut Reader<'_>, n: usize| -> Result<Vec<Vec<f32>>, Error> {
        (0..layers).map(|_| r.floats(n)).collect()
    };
    let per_layer_matrix =
        |r: &mut Reader<'_>, rows: usize, cols: usize| -> Result<Vec<Matrix>, Error> {
            (0..layers).map(|_| r.matrix(rows, cols)).collect()
        };
    let rms_attention = per_layer(&mut r, dim)?;
    let wq = per_layer_matrix(&mut r, dim, dim)?;
    let wk = per_layer_matrix(&mut r, kv_dim, dim)?;
    let wv = per_layer_matrix(&mut r, kv_dim, dim)?;
    let wo = per_layer_matrix(&mut r, dim, dim)?;
    let rms_ffn = per_layer(&mut r, dim)?;
    let w1 = per_layer_matrix(&mut r, hidden_dim, dim)?;
    let w2 = per_layer_matrix(&mut r, dim, hidden_dim)?;
    let w3 = per_layer_matrix(&mut r, hidden_dim, dim)?;
    let rms_final = r.floats(dim)?;
    // Legacy precomputed RoPE tables (real and imaginary halves); unused
    // because RoPE is computed from the position.
    r.take(
        seq_len
            .checked_mul(head_dim)
            .and_then(|n| n.checked_mul(4))
            .ok_or(Error::Overflow)?,
    )?;
    let embedding = Matrix::f32(vocab, dim, embedding)?;
    let classifier = if shared_classifier {
        embedding.clone()
    } else {
        r.matrix(vocab, dim)?
    };
    if r.offset != bytes.len() {
        return Err(Error::Malformed("trailing bytes after checkpoint"));
    }
    let mut layer_vec = Vec::with_capacity(layers);
    let mut wq = wq.into_iter();
    let mut wk = wk.into_iter();
    let mut wv = wv.into_iter();
    let mut wo = wo.into_iter();
    let mut w1 = w1.into_iter();
    let mut w2 = w2.into_iter();
    let mut w3 = w3.into_iter();
    for (rms_attention, rms_ffn) in rms_attention.into_iter().zip(rms_ffn) {
        let missing = || Error::Malformed("layer");
        layer_vec.push(Layer {
            rms_attention,
            wq: wq.next().ok_or_else(missing)?,
            wk: wk.next().ok_or_else(missing)?,
            wv: wv.next().ok_or_else(missing)?,
            wo: wo.next().ok_or_else(missing)?,
            rms_ffn,
            w1: w1.next().ok_or_else(missing)?,
            w2: w2.next().ok_or_else(missing)?,
            w3: w3.next().ok_or_else(missing)?,
        });
    }
    Ok(Model {
        config,
        embedding,
        layers: layer_vec,
        rms_final,
        classifier,
    })
}

pub(crate) fn tokenizer(bytes: &[u8], vocab: usize) -> Result<Tokenizer, Error> {
    if vocab == 0 || vocab > MAX_VOCAB {
        return Err(Error::Limit("vocab"));
    }
    let mut r = Reader { bytes, offset: 0 };
    let max_token_length = positive(r.i32()?, "max_token_length", MAX_TOKEN_LENGTH)?;
    let mut pieces = Vec::with_capacity(vocab);
    let mut scores = Vec::with_capacity(vocab);
    for _ in 0..vocab {
        let score = r.f32()?;
        if !score.is_finite() {
            return Err(Error::NonFinite);
        }
        let len = positive(r.i32()?, "token length", max_token_length)?;
        pieces.push(r.take(len)?.to_vec());
        scores.push(score);
    }
    if r.offset != bytes.len() {
        return Err(Error::Malformed("trailing bytes after tokenizer"));
    }
    Ok(Tokenizer {
        pieces,
        scores,
        max_token_length,
    })
}
