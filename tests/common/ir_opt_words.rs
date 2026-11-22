//! The `ir_opt` swap's encoder and decoder (self-hosting SH3 lane 9,
//! #1248), shared by tests/self_host_ir_opt.rs and the hybrid build's `opt`
//! hook (crates/vesper-hybrid/src/hybrid.rs).
//!
//! The input is `ir_words::program_input` (names, types, world, program)
//! followed by the mode and the world's answers for `@fip` reuse
//! (`Facts`): `fip::active`, `caller_slot_functions`, `box_inner`,
//! `pattern_box_slots` and `slot_key`, which the package does not port.
//! The output is decoded by `finish`, which also runs the Rust tail of
//! `optimize` (`ir::thread_tokens`, then `ir_check::validate`).
#![allow(dead_code)]

use super::ir_words;
use std::collections::BTreeMap;
use vesper::ast::{Generic, Type};
use vesper::check::World;
use vesper::fip::FipOp;
use vesper::ir::{Op, Program};

// ------------------------------------------------------------------ facts

/// `fip::slot_key` (pub(crate) in src/fip.rs), transcribed.
pub fn slot_key(world: &World, generics: &[Generic], t: &Type) -> Result<(String, u32), String> {
    fn has_generic(t: &Type, generics: &[Generic]) -> bool {
        match t {
            Type::Named(n, args) => {
                generics.iter().any(|g| !g.row && &g.name == n)
                    || args.iter().any(|x| has_generic(x, generics))
            }
            Type::Tuple(ts) => ts.iter().any(|x| has_generic(x, generics)),
            Type::Function { params, result, .. } => {
                params.iter().any(|(_, t)| has_generic(t, generics))
                    || has_generic(result, generics)
            }
            Type::Row(_) | Type::IntLiteral(_) | Type::Infer => false,
        }
    }
    let norm = vesper::numeric::default_type(t);
    if !has_generic(&norm, generics) {
        let layout = vesper::layout::Layout::of(world, &norm)?;
        Ok((format!("size:{}", layout.size), layout.size))
    } else {
        Ok((format!("type:{norm}"), 0))
    }
}

/// The world's answers for reuse.ves, and the `slot_key` error messages
/// its error indices name.
#[derive(Default)]
pub struct Facts {
    keys: BTreeMap<String, u64>,
    errors: Vec<String>,
}

impl Facts {
    fn key(&mut self, k: String) -> u64 {
        let n = self.keys.len() as u64;
        *self.keys.entry(k).or_insert(n)
    }

    fn error(&mut self, e: String) -> u64 {
        self.errors.push(e);
        self.errors.len() as u64 - 1
    }

    /// A `Box::new` or caller-slot call of type `ty`: nothing, a key or an
    /// error.
    fn need(&mut self, world: &World, generics: &[Generic], ty: &Type, out: &mut Vec<u64>) {
        match vesper::fip::box_inner(world, ty) {
            None => out.push(0),
            Some(inner) => match slot_key(world, generics, inner) {
                Ok((k, _)) => {
                    let k = self.key(k);
                    out.extend([3, k]);
                }
                Err(e) => {
                    let e = self.error(e);
                    out.extend([4, e]);
                }
            },
        }
    }

    pub fn words(&mut self, world: &World, program: &Program) -> Vec<u64> {
        if !vesper::fip::active(world) {
            return vec![0];
        }
        let caller = vesper::fip::caller_slot_functions(world);
        let mut body = vec![];
        for f in program.functions.values() {
            let Some(src) = world.functions.get(&f.name).filter(|s| s.fip) else {
                body.push(0);
                continue;
            };
            body.push(1);
            let ret = vesper::fip::box_inner(world, &src.result)
                .and_then(|inner| slot_key(world, &src.generics, inner).ok());
            match ret {
                Some((k, _)) => {
                    let k = self.key(k);
                    body.push(k + 1);
                }
                None => body.push(0),
            }
            let n: usize = f.blocks.iter().map(|b| b.instructions.len()).sum();
            body.push(n as u64);
            for b in &f.blocks {
                for i in &b.instructions {
                    match &i.op {
                        Op::Bind { pattern, value } => {
                            let Some(ty) = f.values.get(value) else {
                                body.push(0);
                                continue;
                            };
                            let slots = vesper::fip::pattern_box_slots(world, pattern, ty);
                            let mut rec = vec![];
                            let mut err = None;
                            for (_, inner) in &slots {
                                match slot_key(world, &src.generics, inner) {
                                    Ok((k, size)) => {
                                        let k = self.key(k);
                                        rec.extend([k, size as u64]);
                                    }
                                    Err(e) => {
                                        err = Some(e);
                                        break;
                                    }
                                }
                            }
                            if let Some(e) = err {
                                let e = self.error(e);
                                body.extend([2, e]);
                            } else {
                                body.extend([1, slots.len() as u64]);
                                body.extend(rec);
                            }
                        }
                        Op::Fip {
                            op: FipOp::BoxNew, ..
                        } => self.need(world, &src.generics, &i.ty, &mut body),
                        Op::Call { callee, .. } if caller.contains(callee) => {
                            self.need(world, &src.generics, &i.ty, &mut body)
                        }
                        _ => body.push(0),
                    }
                }
            }
        }
        let mut out = vec![1, self.keys.len() as u64];
        out.extend(body);
        out
    }
}

// ------------------------------------------------------------------ the pass

pub const MODE_OPTIMIZE: u64 = 0;
pub const MODE_REUSE: u64 = 1;

/// The entry's input and what decoding its output needs.
pub struct Prepared {
    pub input: Vec<u8>,
    pub pool: ir_words::Pool,
    pub errors: Vec<String>,
}

pub fn prepare(world: &World, program: &Program, mode: u64) -> Option<Prepared> {
    let (mut input, pool) = ir_words::program_input(world, program)?;
    let mut facts = Facts::default();
    let mut tail = vec![mode];
    tail.extend(facts.words(world, program));
    for w in tail {
        input.extend(w.to_le_bytes());
    }
    Some(Prepared {
        input,
        pool,
        errors: facts.errors,
    })
}

/// The entry's words as the pass's result: the count or the error, and the
/// rewritten program. `optimize` runs Rust's tail (token threading and the
/// IR check) on the decoded program.
pub fn finish(
    world: &World,
    prep: &Prepared,
    words: &[u64],
    mode: u64,
) -> (Result<usize, String>, Option<Program>) {
    let (status, error, count, np) = (words[0], words[1], words[2], words[3] as usize);
    match status {
        0 => {}
        1 => return (Err("read lacks state evidence".into()), None),
        // egglog's own message is not reproduced; the prefix is compared.
        2 => return (Err(REFUSAL_PREFIX.into()), None),
        _ => return (Err(prep.errors[error as usize].clone()), None),
    }
    let mut pool = prep.pool.clone();
    for k in 0..np {
        let w = &words[4 + 3 * k..7 + 3 * k];
        pool.items.push(ir_words::Payload::Fip(FipOp::BoxReuse {
            bind_dest: w[0] as u32,
            slot_index: w[1] as usize,
            size: w[2] as u32,
        }));
    }
    let mut program = ir_words::decode_program(&words[4 + 3 * np..], &pool);
    if mode == MODE_OPTIMIZE {
        vesper::ir::thread_tokens(&mut program);
        let errors = vesper::ir_check::validate(world, &program);
        if !errors.is_empty() {
            return (
                Err(format!(
                    "optimized IR failed ownership validation: {errors:?}"
                )),
                None,
            );
        }
    }
    (Ok(count as usize), Some(program))
}

/// Status 2 of the entry: egglog would refuse a block. Its message is
/// egglog's own, which the package does not reproduce; callers compare this
/// prefix (the test) or ask Rust for the message (the hybrid).
pub const REFUSAL_PREFIX: &str = "effect equality saturation: ";

/// `ir_opt::optimize` through the package: `call` runs the entry on the
/// input bytes. `None` leaves the pass to Rust: the program does not fit
/// the Place IR words, or egglog would refuse a block (its message is
/// egglog's). On success the optimized program replaces `program`.
pub fn optimize_with(
    world: &World,
    program: &mut Program,
    call: impl FnOnce(&[u8]) -> Vec<u64>,
) -> Option<Result<usize, String>> {
    let prep = prepare(world, program, MODE_OPTIMIZE)?;
    let words = call(&prep.input);
    match finish(world, &prep, &words, MODE_OPTIMIZE) {
        (Err(e), _) if e == REFUSAL_PREFIX => None,
        (Err(e), _) => Some(Err(e)),
        (Ok(n), Some(p)) => {
            *program = p;
            Some(Ok(n))
        }
        (Ok(_), None) => None,
    }
}
