//! Scalar, non-delta StreamVByte u32 codec: ceil(count/4) control bytes first,
//! then little-endian data. Two-bit tags select lengths 1,2,3,4 in lane order.
//! The value count is out-of-band, as in the original raw StreamVByte format.
//! No unsafe loads, input padding, heap storage, SIMD or speed claim is used.
use crate::binary::{Error, ErrorKind, Result, add, bound, mul, work};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_values: usize,
    pub max_bytes: usize,
    /// Covers control inspection, value inspection and both data passes.
    pub max_work: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_values: 262_144,
            max_bytes: 2 * 1024 * 1024,
            max_work: 8 * 1024 * 1024,
        }
    }
}

/// Checked maximum, including controls. Usable without allocating an input.
pub fn max_encoded_size(count: usize) -> Result<usize> {
    add(controls(count)?, mul(count, 4)?)
}
fn controls(count: usize) -> Result<usize> {
    add(count / 4, usize::from(!count.is_multiple_of(4)))
}
fn width(value: u32) -> usize {
    ((32 - value.leading_zeros()).max(1) as usize).div_ceil(8)
}

/// Exact encoded length with resource checks, without writing or allocating.
pub fn encoded_size(values: &[u32], limits: Limits) -> Result<usize> {
    bound(values.len(), limits.max_values, ErrorKind::CountLimit)?;
    let control = controls(values.len())?;
    let mut size = control;
    let mut remaining = limits.max_work;
    work(&mut remaining, add(control, mul(values.len(), 2)?)?)?;
    for &value in values {
        let n = width(value);
        work(&mut remaining, mul(n, 2)?)?;
        size = add(size, n)?;
        bound(size, limits.max_bytes, ErrorKind::ByteLimit)?;
    }
    bound(size, limits.max_bytes, ErrorKind::ByteLimit)?;
    Ok(size)
}

/// Every capacity/resource failure preserves the complete caller buffer.
/// Successful writes touch exactly the returned prefix.
pub fn encode_into(values: &[u32], output: &mut [u8], limits: Limits) -> Result<usize> {
    let size = encoded_size(values, limits)?;
    if output.len() < size {
        return Err(Error::new(ErrorKind::OutputTooSmall, 0));
    }
    let control = controls(values.len())?;
    output[..control].fill(0);
    let mut pos = control;
    for (i, &value) in values.iter().enumerate() {
        let n = width(value);
        output[i / 4] |= ((n - 1) as u8) << ((i % 4) * 2);
        output[pos..pos + n].copy_from_slice(&value.to_le_bytes()[..n]);
        pos += n;
    }
    Ok(size)
}

/// Validates exact frame length and all control-selected data sizes before
/// writing any values. Unused tail control bits are ignored (not additional
/// values), and non-minimal widths are accepted for raw-format interoperability.
/// Extra data bytes are rejected; count is explicitly supplied by the caller.
pub fn decode_into(
    input: &[u8],
    count: usize,
    output: &mut [u32],
    limits: Limits,
) -> Result<usize> {
    bound(count, limits.max_values, ErrorKind::CountLimit)?;
    bound(input.len(), limits.max_bytes, ErrorKind::ByteLimit)?;
    let control = controls(count)?;
    if input.len() < control {
        return Err(Error::new(ErrorKind::Truncated, input.len()));
    }
    if output.len() < count {
        return Err(Error::new(ErrorKind::OutputTooSmall, 0));
    }
    let mut remaining = limits.max_work;
    work(&mut remaining, add(control, mul(count, 2)?)?)?;
    let mut end = control;
    for i in 0..count {
        let n = usize::from((input[i / 4] >> ((i % 4) * 2)) & 3) + 1;
        work(&mut remaining, mul(n, 2)?)?;
        end = add(end, n)?;
        if end > input.len() {
            return Err(Error::new(ErrorKind::Truncated, input.len()));
        }
    }
    if end != input.len() {
        return Err(Error::new(ErrorKind::TrailingBytes, end));
    }
    let mut pos = control;
    for (i, value) in output[..count].iter_mut().enumerate() {
        let n = usize::from((input[i / 4] >> ((i % 4) * 2)) & 3) + 1;
        let mut bytes = [0; 4];
        bytes[..n].copy_from_slice(&input[pos..pos + n]);
        *value = u32::from_le_bytes(bytes);
        pos += n;
    }
    Ok(count)
}
