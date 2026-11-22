//! The Rust half of `@vesper/codegen-export`'s input (SH4 lane 12, #1286):
//! a [`LibraryInterface`] and [`BindingOptions`] as little-endian `u64`
//! words in one `Str`, which `binding_text` in
//! packages/vesper-codegen-export/src/input.ves reads back.
//!
//! Layout (every item one word unless noted):
//!
//! - names: `count`, then per name its byte length and the bytes, zero
//!   padded to a word (the check-data layout). An optional name is 0 for
//!   none, else its index + 1.
//! - types: `count`, then the [`AbiType`] nodes in post-order (a node's
//!   parts first), hash-consed, so equal types have equal indices. A node
//!   is `kind, name, flag, n, child * n` with kinds `K_*` below; `name` is
//!   the scalar, named, object or extern name (0 when it has none), and
//!   `flag` is `Str`'s `owned`.
//! - the header: abi, library, option_mirrors, the optional types path.
//! - runtime: present; then seq_new, seq_free, live_allocations, optional
//!   box_new.
//! - types (the interface's `BTreeMap` order): `count`, then per decl:
//!   name, is_enum, words, handle, linear, optional drop symbol,
//!   owns_sequence, mirror (0 none, 1 `repr(C)`, 2 converted), optional
//!   reason, variant count; per variant name and field count; per field
//!   name, type, offset.
//! - functions: `count`, then per function: name, symbol, param count; per
//!   param name, type, pointer, pass (0 none, 1 in, 2 inout, 3 sink, 4 any
//!   other spelling, as an optional name next), in_place; then result type,
//!   result_words, result_pointer, error count and types, fail record
//!   words (0 for none, else n + 1), optional instance_of, foreign count
//!   and names, result_in_place.
//! - foreign libraries: `count`, then per library: name, handler, symbol
//!   count and symbols, optional rust_path.
use std::collections::BTreeMap;
use vesper::native::export::{AbiType, BindingOptions, LibraryInterface, Mirror};

pub const K_SCALAR: u64 = 0;
pub const K_TUPLE: u64 = 1;
pub const K_NAMED: u64 = 2;
pub const K_SEQ: u64 = 3;
pub const K_SLICE: u64 = 4;
pub const K_BOX: u64 = 5;
pub const K_STR: u64 = 6;
pub const K_OBJECT: u64 = 7;
pub const K_EXTERN: u64 = 8;

#[derive(Default)]
struct Enc {
    names: BTreeMap<String, u64>,
    name_list: Vec<String>,
    types: BTreeMap<Vec<u64>, u64>,
    type_words: Vec<u64>,
    out: Vec<u64>,
}

impl Enc {
    fn name(&mut self, s: &str) -> u64 {
        if let Some(i) = self.names.get(s) {
            return *i;
        }
        let i = self.name_list.len() as u64;
        self.names.insert(s.into(), i);
        self.name_list.push(s.into());
        i
    }

    fn opt(&mut self, s: Option<&str>) -> u64 {
        s.map_or(0, |s| self.name(s) + 1)
    }

    fn ty(&mut self, t: &AbiType) -> u64 {
        let (kind, name, flag, children): (u64, u64, u64, Vec<&AbiType>) = match t {
            AbiType::Scalar { name } => (K_SCALAR, self.name(name), 0, vec![]),
            AbiType::Tuple { items } => (K_TUPLE, 0, 0, items.iter().collect()),
            AbiType::Named { name, args } => (K_NAMED, self.name(name), 0, args.iter().collect()),
            AbiType::Seq { elem } => (K_SEQ, 0, 0, vec![&**elem]),
            AbiType::Slice { elem } => (K_SLICE, 0, 0, vec![&**elem]),
            AbiType::Box { inner } => (K_BOX, 0, 0, vec![&**inner]),
            AbiType::Str { owned } => (K_STR, 0, *owned as u64, vec![]),
            AbiType::Object { name } => (K_OBJECT, self.name(name), 0, vec![]),
            AbiType::Extern { name } => (K_EXTERN, self.name(name), 0, vec![]),
        };
        let ids: Vec<u64> = children.into_iter().map(|c| self.ty(c)).collect();
        let mut key = vec![kind, name, flag, ids.len() as u64];
        key.extend(ids);
        if let Some(i) = self.types.get(&key) {
            return *i;
        }
        let i = self.types.len() as u64;
        self.type_words.extend(&key);
        self.types.insert(key, i);
        i
    }

    fn push(&mut self, x: u64) {
        self.out.push(x);
    }

    fn interface(&mut self, i: &LibraryInterface, options: &BindingOptions) {
        let w = [
            self.name(&i.abi),
            self.name(&i.library),
            i.option_mirrors as u64,
            self.opt(options.types_path.as_deref()),
        ];
        self.out.extend(w);
        match &i.runtime {
            None => self.push(0),
            Some(r) => {
                let w = [
                    1,
                    self.name(&r.seq_new),
                    self.name(&r.seq_free),
                    self.name(&r.live_allocations),
                    self.opt(r.box_new.as_deref()),
                ];
                self.out.extend(w);
            }
        }
        self.push(i.types.len() as u64);
        for d in i.types.values() {
            let (mirror, reason) = match &d.mirror {
                None => (0, None),
                Some(Mirror::ReprC) => (1, None),
                Some(Mirror::Converted { reason }) => (2, Some(reason.as_str())),
            };
            let w = [
                self.name(&d.name),
                d.is_enum as u64,
                d.words as u64,
                d.handle as u64,
                d.linear as u64,
                self.opt(d.drop_symbol.as_deref()),
                d.owns_sequence as u64,
                mirror,
                self.opt(reason),
                d.variants.len() as u64,
            ];
            self.out.extend(w);
            for v in &d.variants {
                let w = [self.name(&v.name), v.fields.len() as u64];
                self.out.extend(w);
                for f in &v.fields {
                    let w = [self.name(&f.name), self.ty(&f.ty), f.offset as u64];
                    self.out.extend(w);
                }
            }
        }
        self.push(i.functions.len() as u64);
        for f in &i.functions {
            let w = [
                self.name(&f.name),
                self.name(&f.symbol),
                f.params.len() as u64,
            ];
            self.out.extend(w);
            for p in &f.params {
                let (pass, other) = match p.pass.as_deref() {
                    None => (0, None),
                    Some("in") => (1, None),
                    Some("inout") => (2, None),
                    Some("sink") => (3, None),
                    Some(other) => (4, Some(other)),
                };
                let w = [self.name(&p.name), self.ty(&p.ty), p.pointer as u64, pass];
                self.out.extend(w);
                if pass == 4 {
                    let o = self.opt(other);
                    self.push(o);
                }
                self.push(p.in_place as u64);
            }
            let w = [
                self.ty(&f.result),
                f.result_words as u64,
                f.result_pointer as u64,
                f.errors.len() as u64,
            ];
            self.out.extend(w);
            for e in &f.errors {
                let t = self.ty(e);
                self.push(t);
            }
            let w = [
                f.fail_record_words.map_or(0, |n| n as u64 + 1),
                self.opt(f.instance_of.as_deref()),
                f.foreign.len() as u64,
            ];
            self.out.extend(w);
            for l in &f.foreign {
                let n = self.name(l);
                self.push(n);
            }
            self.push(f.result_in_place as u64);
        }
        self.push(i.foreign.len() as u64);
        for l in &i.foreign {
            let w = [
                self.name(&l.name),
                self.name(&l.handler),
                l.symbols.len() as u64,
            ];
            self.out.extend(w);
            for s in &l.symbols {
                let n = self.name(s);
                self.push(n);
            }
            let r = self.opt(l.rust_path.as_deref());
            self.push(r);
        }
    }

    fn finish(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(8 * (self.out.len() + self.type_words.len()) + 4096);
        let put = |b: &mut Vec<u8>, x: u64| b.extend(x.to_le_bytes());
        put(&mut bytes, self.name_list.len() as u64);
        for n in &self.name_list {
            put(&mut bytes, n.len() as u64);
            bytes.extend(n.as_bytes());
            while !bytes.len().is_multiple_of(8) {
                bytes.push(0);
            }
        }
        put(&mut bytes, self.types.len() as u64);
        for x in &self.type_words {
            put(&mut bytes, *x);
        }
        for x in &self.out {
            put(&mut bytes, *x);
        }
        bytes
    }
}

/// The input of `binding_text`: `interface` and `options` as words.
pub fn binding_input(interface: &LibraryInterface, options: &BindingOptions) -> Vec<u8> {
    let mut e = Enc::default();
    e.interface(interface, options);
    e.finish()
}
