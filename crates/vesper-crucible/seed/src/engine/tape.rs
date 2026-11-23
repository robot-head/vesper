//! Frozen Rust choice-tape key and text codec fallback for `vesper-crucible-seed`.

use crate::tape::{ChoiceTape, MAX_TAPE_LEN, ShrinkKey, TAPE_HEADER, TapeError};
use std::cmp::Ordering;

pub(crate) fn key_of(values: &[u64]) -> ShrinkKey {
    ShrinkKey {
        len: values.len(),
        sum: values.iter().map(|&v| u128::from(v)).sum(),
    }
}

pub(crate) fn key_smaller_than(lhs: ShrinkKey, rhs: ShrinkKey) -> bool {
    match lhs.len.cmp(&rhs.len) {
        Ordering::Less => true,
        Ordering::Equal => lhs.sum < rhs.sum,
        Ordering::Greater => false,
    }
}

pub(crate) fn to_text(values: &[u64]) -> String {
    let mut out = String::with_capacity(TAPE_HEADER.len() + 1 + values.len() * 17);
    out.push_str(TAPE_HEADER);
    out.push('\n');
    for value in values {
        out.push_str(&format!("{value:x}\n"));
    }
    out
}

pub(crate) fn from_text(text: &str) -> Result<ChoiceTape, TapeError> {
    let mut lines = text.lines();
    let header = loop {
        match lines.next() {
            None => return Err(TapeError::MissingHeader),
            Some(line) if line.trim().is_empty() => continue,
            Some(line) => break line.trim(),
        }
    };
    if header != TAPE_HEADER {
        let shown: String = header.chars().take(64).collect();
        return Err(TapeError::UnknownHeader(shown));
    }
    let mut values = Vec::new();
    for line in lines {
        let token = line.trim();
        if token.is_empty() {
            continue;
        }
        if values.len() == MAX_TAPE_LEN {
            return Err(TapeError::TooLong {
                limit: MAX_TAPE_LEN,
            });
        }
        let index = values.len();
        // `from_str_radix` would accept a leading `+`; check digits first.
        let valid = !token.is_empty()
            && token.len() <= 16
            && token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !valid {
            return Err(TapeError::BadValue { index });
        }
        let value = u64::from_str_radix(token, 16).map_err(|_| TapeError::BadValue { index })?;
        values.push(value);
    }
    Ok(ChoiceTape::new(values))
}
