//! One shared world and word answers across the hybrid's checker swaps
//! (self-hosting SH5 lane 10, #1333).
//!
//! `modules/checks` links the layout and the IR check into one Vesper
//! library, so one `Checks` value per compilation holds the one decoded
//! world both swaps read. Its layout answers (format 2) name the input's
//! own names and types by index; [`decode_layout`] reads them back from the
//! compilation's `WorldLog` tables, straight out of the value's `out`
//! buffer (no copy of the answer on either side).
use super::check_words::{Adts, Cursor, WorldLog};
use vesper::ast::{Effect, Effects, Mode, Pass, Type};
use vesper::check::World;
use vesper::layout::{Layout, LayoutField, LayoutKind, LayoutVariant};
use vesper_port_abi::SeqRef;

/// The answer format `modules/checks` writes (its `format()`).
pub const FORMAT: u64 = 2;

/// Deepest layout a stream may hold (Vesper refuses layouts deeper than
/// 128 levels; a deeper stream is corrupt).
const MAX_DEPTH: usize = 256;

/// The shared input of one compilation: what the Vesper value holds of the
/// compilation's `WorldLog`, and the value.
pub struct Shared {
    pub cur: Cursor,
    pub checks: super::checks_binding::Checks,
}

impl Shared {
    /// Opens the shared input: every ADT (in name order) and every effect
    /// crosses once, so both swaps' later parts carry only new names,
    /// types and their own words. `None` when an ADT cannot be encoded
    /// (the swaps then keep their own inputs).
    pub fn open(log: &mut WorldLog, world: &World) -> Option<Self> {
        let mut cur = Cursor::default();
        let part = log.part(world, &mut cur, Adts::All, true, |_| vec![])?;
        let mut checks = super::checks_binding::Checks::new();
        checks.extend(&part);
        Some(Self { cur, checks })
    }
}

/// Reads a format-2 answer in place.
struct Reader<'a> {
    words: SeqRef<'a, u64>,
    pos: usize,
}

impl Reader<'_> {
    fn next(&mut self) -> u64 {
        let x = self.words.get(self.pos).unwrap_or(0);
        self.pos += 1;
        x
    }

    fn count(&mut self) -> usize {
        // A count never exceeds the words left; clamp a corrupt one.
        (self.next() as usize).min(self.words.len().saturating_sub(self.pos))
    }

    fn text(&mut self) -> String {
        let len = (self.next() as usize).min(8 * self.words.len().saturating_sub(self.pos));
        let mut bytes = Vec::with_capacity(len + 8);
        while bytes.len() < len {
            bytes.extend_from_slice(&self.next().to_le_bytes());
        }
        bytes.truncate(len);
        String::from_utf8(bytes)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
    }
}

/// The input's tables and the answer's own, for reference lookups: `2 * i`
/// is the input's entry `i`, `2 * k + 1` the answer's own entry `k`.
struct Tables<'a> {
    names: &'a [String],
    types: &'a [Type],
    own_names: Vec<String>,
    own_types: Vec<Type>,
}

impl Tables<'_> {
    fn name(&self, r: u64) -> String {
        let i = (r / 2) as usize;
        let found = if r.is_multiple_of(2) {
            self.names.get(i)
        } else {
            self.own_names.get(i)
        };
        found.cloned().unwrap_or_default()
    }

    fn opt(&self, r: u64) -> Option<String> {
        (r != 0).then(|| self.name(r - 1))
    }

    fn ty(&self, r: u64) -> Type {
        let i = (r / 2) as usize;
        let found = if r.is_multiple_of(2) {
            self.types.get(i)
        } else {
            self.own_types.get(i)
        };
        found.cloned().unwrap_or(Type::Infer)
    }

    fn row(&self, r: &mut Reader) -> Effects {
        let n = r.count();
        let mut instances = Vec::with_capacity(n);
        for _ in 0..n {
            let label = self.opt(r.next());
            let name = self.name(r.next());
            let k = r.count();
            let args = (0..k).map(|_| self.ty(r.next())).collect();
            instances.push(Effect { label, name, args });
        }
        let tail = self.opt(r.next());
        Effects { instances, tail }
    }

    fn read_type(&self, r: &mut Reader) -> Type {
        match r.next() {
            0 => {
                let negative = r.next() != 0;
                let magnitude = r.next() as i128;
                Type::IntLiteral(if negative { -magnitude } else { magnitude })
            }
            1 => {
                let n = self.name(r.next());
                let k = r.count();
                Type::Named(n, (0..k).map(|_| self.ty(r.next())).collect())
            }
            2 => {
                let k = r.count();
                Type::Tuple((0..k).map(|_| self.ty(r.next())).collect())
            }
            3 => {
                let k = r.count();
                let params = (0..k)
                    .map(|_| {
                        let p = pass_of(r.next());
                        (p, self.ty(r.next()))
                    })
                    .collect();
                let result = Box::new(self.ty(r.next()));
                let effects = self.row(r);
                let mode = mode_of(r.next());
                Type::Function {
                    params,
                    result,
                    effects,
                    mode,
                }
            }
            4 => Type::Row(self.row(r)),
            _ => Type::Infer,
        }
    }

    fn fields(&self, r: &mut Reader, depth: usize) -> Vec<LayoutField> {
        let n = r.count();
        (0..n)
            .map(|_| {
                let name = self.name(r.next());
                let ty = self.ty(r.next());
                let offset = r.next() as i64 as i32;
                let layout = Box::new(self.layout(r, depth + 1));
                LayoutField {
                    name,
                    ty,
                    offset,
                    layout,
                }
            })
            .collect()
    }

    fn layout(&self, r: &mut Reader, depth: usize) -> Layout {
        let ty = self.ty(r.next());
        let size = r.next() as u32;
        let kind = r.next();
        if depth > MAX_DEPTH {
            return Layout {
                ty,
                size,
                kind: LayoutKind::Scalar,
            };
        }
        let kind = match kind {
            1 => LayoutKind::Product(self.fields(r, depth)),
            2 => {
                let n = r.count();
                LayoutKind::Enum(
                    (0..n)
                        .map(|_| {
                            let name = self.name(r.next());
                            LayoutVariant {
                                name,
                                fields: self.fields(r, depth),
                            }
                        })
                        .collect(),
                )
            }
            _ => LayoutKind::Scalar,
        };
        Layout { ty, size, kind }
    }
}

fn pass_of(x: u64) -> Pass {
    match x {
        1 => Pass::Inout,
        2 => Pass::Sink,
        _ => Pass::In,
    }
}

fn mode_of(x: u64) -> Mode {
    match x {
        1 => Mode::Affine,
        2 => Mode::Linear,
        _ => Mode::Copy,
    }
}

/// Reads a format-2 layout answer, whose references name `log`'s names and
/// types, back into `Layout::of`'s result.
pub fn decode_layout(words: SeqRef<'_, u64>, log: &WorldLog) -> Result<Layout, String> {
    let mut r = Reader { words, pos: 0 };
    if r.next() != FORMAT {
        return Err(r.text());
    }
    let mut t = Tables {
        names: &log.enc.name_list,
        types: &log.enc.type_list,
        own_names: vec![],
        own_types: vec![],
    };
    let n = r.count();
    t.own_names = (0..n).map(|_| r.text()).collect();
    let k = r.count();
    for _ in 0..k {
        let ty = t.read_type(&mut r);
        t.own_types.push(ty);
    }
    Ok(t.layout(&mut r, 0))
}
