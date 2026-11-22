//! Cranelift SIMD lowering of the Place-IR vector operations (§14.3, lane
//! 28). A child module of `native`.
//!
//! A vector or mask is an aggregate: a pointer to its packed lane bytes
//! (see [`crate::vector::layout_words`]). Operations load it as one, two or
//! four 128-bit Cranelift vectors (`i8x16`, `i32x4`, `i64x2`, `f32x4`,
//! `f64x2`),
//! compute chunk by chunk and store the result into a fresh stack slot.
//! Cranelift 0.125 has no 256- or 512-bit vector types, so wider shapes are
//! chunked 128-bit code under every profile. Mask lanes are all ones or all
//! zeros in the lane width. Scalars follow the backend's convention: integers
//! and bools are zero-extended `i64`, an `f32` is `f32` and an `f64` is `f64`.
//!
//! Slice loads and stores (`SeqOp::Vector`) move whole chunks between the
//! vector and the sequence's packed elements. A `ChunkSeq[f32]` keeps each
//! element in the low half of an eight-byte word (`seq::elem_bytes`), so
//! `f32` lanes are gathered from and scattered to that stride with one byte
//! shuffle per chunk; every other lane type's stride is its lane width.
use super::Codegen;
use crate::vector::{
    BinOp, CmpOp, Conv, Elem, Intrinsic, MaskOp, ReduceOp, Shape, ShiftOp, SliceIo, UnaryOp,
    VectorOp,
};
use cranelift_codegen::ir::{
    self, ConstantData, InstBuilder, MemFlags,
    condcodes::{FloatCC, IntCC},
    types,
};
use cranelift_module::Module;

fn chunk_type(elem: Elem) -> ir::Type {
    match elem {
        Elem::U8 => types::I8X16,
        Elem::U32 => types::I32X4,
        Elem::U64 | Elem::I64 => types::I64X2,
        Elem::F32 => types::F32X4,
        Elem::F64 => types::F64X2,
    }
}
/// The integer chunk type masks of `elem` use.
fn mask_type(elem: Elem) -> ir::Type {
    match elem {
        Elem::F32 => types::I32X4,
        Elem::F64 => types::I64X2,
        e => chunk_type(e),
    }
}
fn lane_type(elem: Elem) -> ir::Type {
    match elem {
        Elem::U8 => types::I8,
        Elem::U32 => types::I32,
        Elem::U64 | Elem::I64 => types::I64,
        Elem::F32 => types::F32,
        Elem::F64 => types::F64,
    }
}
fn int_cc(op: CmpOp, signed: bool) -> IntCC {
    match (op, signed) {
        (CmpOp::Eq, _) => IntCC::Equal,
        (CmpOp::Ne, _) => IntCC::NotEqual,
        (CmpOp::Lt, true) => IntCC::SignedLessThan,
        (CmpOp::Le, true) => IntCC::SignedLessThanOrEqual,
        (CmpOp::Gt, true) => IntCC::SignedGreaterThan,
        (CmpOp::Ge, true) => IntCC::SignedGreaterThanOrEqual,
        (CmpOp::Lt, false) => IntCC::UnsignedLessThan,
        (CmpOp::Le, false) => IntCC::UnsignedLessThanOrEqual,
        (CmpOp::Gt, false) => IntCC::UnsignedGreaterThan,
        (CmpOp::Ge, false) => IntCC::UnsignedGreaterThanOrEqual,
    }
}
fn float_cc(op: CmpOp) -> FloatCC {
    match op {
        CmpOp::Eq => FloatCC::Equal,
        CmpOp::Ne => FloatCC::NotEqual,
        CmpOp::Lt => FloatCC::LessThan,
        CmpOp::Le => FloatCC::LessThanOrEqual,
        CmpOp::Gt => FloatCC::GreaterThan,
        CmpOp::Ge => FloatCC::GreaterThanOrEqual,
    }
}
fn little() -> MemFlags {
    MemFlags::new().with_endianness(ir::Endianness::Little)
}

/// The element code of a `numeric::VECTOR` request.
fn request_elem(e: Elem) -> u64 {
    match e {
        Elem::U8 => 0,
        Elem::U32 => 1,
        Elem::U64 => 2,
        Elem::I64 => 3,
        Elem::F32 => 4,
        Elem::F64 => 5,
    }
}

/// The intrinsic code of a `numeric::VECTOR` request, `family << 8 | op`,
/// or `None` for the slice operations and `Isa::supported`.
fn request_intrinsic(i: Intrinsic) -> Option<u64> {
    use Intrinsic as I;
    let (family, op): (u64, u64) = match i {
        I::Splat => (2, 0),
        I::From => (3, 0),
        I::Binary(op) => (
            4,
            match op {
                BinOp::Add => 0,
                BinOp::Sub => 1,
                BinOp::Mul => 2,
                BinOp::Div => 3,
                BinOp::Min => 4,
                BinOp::Max => 5,
                BinOp::And => 6,
                BinOp::Or => 7,
                BinOp::Xor => 8,
                BinOp::SaturatingAdd => 9,
                BinOp::SaturatingSub => 10,
            },
        ),
        I::Not => (5, 0),
        I::Unary(op) => (
            6,
            match op {
                UnaryOp::Sqrt => 0,
                UnaryOp::Abs => 1,
                UnaryOp::Neg => 2,
            },
        ),
        I::MulAdd => (7, 0),
        I::Convert(c) => (
            8,
            match c {
                Conv::ToBits => 0,
                Conv::FromBits => 1,
                Conv::Widen => 2,
                Conv::Narrow => 3,
            },
        ),
        I::Shift(op) => (
            9,
            match op {
                ShiftOp::Shl => 0,
                ShiftOp::Shr => 1,
            },
        ),
        I::Compare(op) => (
            10,
            match op {
                CmpOp::Eq => 0,
                CmpOp::Ne => 1,
                CmpOp::Lt => 2,
                CmpOp::Le => 3,
                CmpOp::Gt => 4,
                CmpOp::Ge => 5,
            },
        ),
        I::Select => (11, 0),
        I::MaskBinary(op) => (
            12,
            match op {
                MaskOp::And => 0,
                MaskOp::Or => 1,
                MaskOp::Xor => 2,
            },
        ),
        I::MaskNot => (13, 0),
        I::MaskCount => (14, 0),
        I::MaskBitmask => (15, 0),
        I::MaskAny => (16, 0),
        I::MaskAll => (17, 0),
        I::Reduce(op) => (
            18,
            match op {
                ReduceOp::Add => 0,
                ReduceOp::Xor => 1,
                ReduceOp::Or => 2,
                ReduceOp::And => 3,
                ReduceOp::Min => 4,
                ReduceOp::Max => 5,
            },
        ),
        I::Extract => (19, 0),
        I::Replace => (20, 0),
        I::Shuffle => (21, 0),
        I::LoadMasked => (22, 0),
        I::InBounds => (26, 0),
        I::Active => (27, 0),
        I::Load | I::Store | I::StoreMasked | I::IsaSupported => return None,
    };
    Some(family << 8 | op)
}

impl<M: Module> Codegen<'_, '_, M> {
    /// Lower one vector operation on its IR operands.
    pub(super) fn vector_op(
        &mut self,
        op: &VectorOp,
        args: &[ir::Value],
    ) -> Result<ir::Value, String> {
        // SH4 swap point (#1285): the `numeric` hook may emit the
        // operation (`codegen_swap::numeric::VECTOR`); none is installed by
        // default.
        if let Some(v) = self.swap_vector(op, args)? {
            return Ok(v);
        }
        let (intrinsic, shape, constants) = match op {
            VectorOp::IsaSupported(p) => {
                let enabled = crate::vector::isa_enabled(self.m.isa(), *p);
                return Ok(self.b.ins().iconst(types::I64, i64::from(enabled)));
            }
            VectorOp::Lanes {
                intrinsic,
                shape,
                constants,
            } => (*intrinsic, *shape, constants.as_slice()),
        };
        let arg = |i: usize| {
            args.get(i)
                .copied()
                .ok_or_else(|| format!("native vector operation lacks operand {i}"))
        };
        let elem = shape.elem;
        let lanes = usize::from(shape.lanes);
        let per = usize::from(shape.lanes_per_chunk());
        let index = |i: usize| -> Result<usize, String> {
            constants
                .get(i)
                .map(|c| usize::from(*c))
                .filter(|c| *c < lanes)
                .ok_or_else(|| "native vector lane index out of range".into())
        };
        use Intrinsic as I;
        Ok(match intrinsic {
            I::Splat => {
                let lane = self.lane_of(arg(0)?, elem);
                let chunk = self.b.ins().splat(chunk_type(elem), lane);
                self.store_chunks(shape, &vec![chunk; shape.chunks()])
            }
            I::From => {
                if args.len() != lanes {
                    return Err("native Vec::from lane count mismatch".into());
                }
                let mut chunks = vec![self.zero_chunk(chunk_type(elem)); shape.chunks()];
                for (i, v) in args.iter().enumerate() {
                    let lane = self.lane_of(*v, elem);
                    chunks[i / per] =
                        self.b
                            .ins()
                            .insertlane(chunks[i / per], lane, (i % per) as u8);
                }
                self.store_chunks(shape, &chunks)
            }
            I::Binary(op) => {
                let a = self.load_chunks(arg(0)?, shape, false);
                let b = self.load_chunks(arg(1)?, shape, false);
                let out = a
                    .into_iter()
                    .zip(b)
                    .map(|(x, y)| self.binary_chunk(op, elem, x, y))
                    .collect::<Vec<_>>();
                self.store_chunks(shape, &out)
            }
            I::Not => {
                let a = self.load_chunks(arg(0)?, shape, false);
                let out = a
                    .into_iter()
                    .map(|x| self.b.ins().bnot(x))
                    .collect::<Vec<_>>();
                self.store_chunks(shape, &out)
            }
            I::Unary(op) => {
                let a = self.load_chunks(arg(0)?, shape, false);
                let out = a
                    .into_iter()
                    .map(|x| match op {
                        UnaryOp::Sqrt => self.b.ins().sqrt(x),
                        UnaryOp::Abs => self.b.ins().fabs(x),
                        UnaryOp::Neg => self.b.ins().fneg(x),
                    })
                    .collect::<Vec<_>>();
                self.store_chunks(shape, &out)
            }
            I::MulAdd => {
                // Cranelift emits `vfmadd` with FMA and NEON `fmla`, and
                // otherwise calls the correctly rounded `fmaf`/`fma` per lane,
                // so every backend rounds once like the evaluator.
                let a = self.load_chunks(arg(0)?, shape, false);
                let b = self.load_chunks(arg(1)?, shape, false);
                let c = self.load_chunks(arg(2)?, shape, false);
                let out = (0..shape.chunks())
                    .map(|i| self.b.ins().fma(a[i], b[i], c[i]))
                    .collect::<Vec<_>>();
                self.store_chunks(shape, &out)
            }
            I::Convert(conv) => {
                let target = conv
                    .result(shape)
                    .ok_or("native vector conversion has no valid result shape")?;
                let a = self.load_chunks(arg(0)?, shape, false);
                let out = self.convert_chunks(conv, target, &a);
                self.store_chunks(target, &out)
            }
            I::Shift(op) => {
                let a = self.load_chunks(arg(0)?, shape, false);
                // Cranelift masks a vector shift amount to the lane width.
                let k = arg(1)?;
                let out = a
                    .into_iter()
                    .map(|x| match op {
                        ShiftOp::Shl => self.b.ins().ishl(x, k),
                        ShiftOp::Shr if elem.is_signed() => self.b.ins().sshr(x, k),
                        ShiftOp::Shr => self.b.ins().ushr(x, k),
                    })
                    .collect::<Vec<_>>();
                self.store_chunks(shape, &out)
            }
            I::Compare(op) => {
                let a = self.load_chunks(arg(0)?, shape, false);
                let b = self.load_chunks(arg(1)?, shape, false);
                let out = a
                    .into_iter()
                    .zip(b)
                    .map(|(x, y)| {
                        if elem.is_float() {
                            self.b.ins().fcmp(float_cc(op), x, y)
                        } else {
                            self.b.ins().icmp(int_cc(op, elem.is_signed()), x, y)
                        }
                    })
                    .collect::<Vec<_>>();
                self.store_chunks(shape, &out)
            }
            I::Select => {
                let m = self.load_chunks(arg(0)?, shape, true);
                let a = self.load_chunks(arg(1)?, shape, false);
                let b = self.load_chunks(arg(2)?, shape, false);
                let out = (0..shape.chunks())
                    .map(|c| {
                        let mask = if elem.is_float() {
                            self.b.ins().bitcast(chunk_type(elem), little(), m[c])
                        } else {
                            m[c]
                        };
                        self.b.ins().bitselect(mask, a[c], b[c])
                    })
                    .collect::<Vec<_>>();
                self.store_chunks(shape, &out)
            }
            I::MaskBinary(op) => {
                let a = self.load_chunks(arg(0)?, shape, true);
                let b = self.load_chunks(arg(1)?, shape, true);
                let out = a
                    .into_iter()
                    .zip(b)
                    .map(|(x, y)| match op {
                        MaskOp::And => self.b.ins().band(x, y),
                        MaskOp::Or => self.b.ins().bor(x, y),
                        MaskOp::Xor => self.b.ins().bxor(x, y),
                    })
                    .collect::<Vec<_>>();
                self.store_chunks(shape, &out)
            }
            I::MaskNot => {
                let a = self.load_chunks(arg(0)?, shape, true);
                let out = a
                    .into_iter()
                    .map(|x| self.b.ins().bnot(x))
                    .collect::<Vec<_>>();
                self.store_chunks(shape, &out)
            }
            I::MaskBitmask => self.bitmask(arg(0)?, shape),
            I::MaskCount => {
                let bits = self.bitmask(arg(0)?, shape);
                self.b.ins().popcnt(bits)
            }
            I::MaskAny | I::MaskAll => {
                let bits = self.bitmask(arg(0)?, shape);
                let flag = if intrinsic == I::MaskAny {
                    self.b.ins().icmp_imm(IntCC::NotEqual, bits, 0)
                } else {
                    let full = if lanes == 64 {
                        -1
                    } else {
                        ((1u64 << lanes) - 1) as i64
                    };
                    self.b.ins().icmp_imm(IntCC::Equal, bits, full)
                };
                self.b.ins().uextend(types::I64, flag)
            }
            I::Reduce(op) => {
                let chunks = self.load_chunks(arg(0)?, shape, false);
                let mut acc = None;
                for i in 0..lanes {
                    let lane = self.b.ins().extractlane(chunks[i / per], (i % per) as u8);
                    let x = self.scalar_of(lane, elem);
                    acc = Some(match acc {
                        None => x,
                        Some(a) => self.reduce_step(op, elem, a, x),
                    });
                }
                acc.ok_or("empty vector reduction")?
            }
            I::Extract => {
                let i = index(0)?;
                let chunks = self.load_chunks(arg(0)?, shape, false);
                let lane = self.b.ins().extractlane(chunks[i / per], (i % per) as u8);
                self.scalar_of(lane, elem)
            }
            I::Replace => {
                let i = index(0)?;
                let mut chunks = self.load_chunks(arg(0)?, shape, false);
                let lane = self.lane_of(arg(1)?, elem);
                chunks[i / per] = self
                    .b
                    .ins()
                    .insertlane(chunks[i / per], lane, (i % per) as u8);
                self.store_chunks(shape, &chunks)
            }
            I::Shuffle => {
                if constants.len() != lanes {
                    return Err("native Vec::shuffle lane count mismatch".into());
                }
                let chunks = self.load_chunks(arg(0)?, shape, false);
                if shape.chunks() == 1 {
                    // One `shuffle` of the chunk's bytes.
                    let width = (elem.bits() / 8) as usize;
                    let mut bytes = Vec::with_capacity(16);
                    for i in 0..lanes {
                        let source = index(i)?;
                        bytes.extend((0..width).map(|b| (source * width + b) as u8));
                    }
                    let bytes_in = self.b.ins().bitcast(types::I8X16, little(), chunks[0]);
                    let immediate = self.b.func.dfg.immediates.push(ConstantData::from(bytes));
                    let shuffled = self.b.ins().shuffle(bytes_in, bytes_in, immediate);
                    let out = self.b.ins().bitcast(chunk_type(elem), little(), shuffled);
                    self.store_chunks(shape, &[out])
                } else {
                    let mut out = chunks.clone();
                    for i in 0..lanes {
                        let source = index(i)?;
                        let lane = self
                            .b
                            .ins()
                            .extractlane(chunks[source / per], (source % per) as u8);
                        out[i / per] = self.b.ins().insertlane(out[i / per], lane, (i % per) as u8);
                    }
                    self.store_chunks(shape, &out)
                }
            }
            I::LoadMasked => self.load_masked(arg(0)?, arg(1)?, shape)?,
            I::InBounds => {
                let len = self.b.ins().load(types::I64, MemFlags::new(), arg(0)?, 0);
                self.active(arg(1)?, len, shape)
            }
            I::Active => self.active(arg(0)?, arg(1)?, shape),
            I::IsaSupported => return Err("native Isa::supported has no shape".into()),
            I::Load | I::Store | I::StoreMasked => {
                return Err(format!(
                    "native {intrinsic:?} is a sequence operation (SeqOp::Vector)"
                ));
            }
        })
    }
    /// The `numeric` hook's request for `op` (`codegen_swap::numeric`
    /// documents the words), or `None` when no hook is in effect or it
    /// declines. It emits nothing itself.
    fn swap_vector(
        &mut self,
        op: &VectorOp,
        args: &[ir::Value],
    ) -> Result<Option<ir::Value>, String> {
        use crate::codegen_swap::{self, Family, numeric};
        if codegen_swap::hook(Family::Numeric).is_none() {
            return Ok(None);
        }
        let (request, words) = match op {
            VectorOp::IsaSupported(p) => {
                let enabled = crate::vector::isa_enabled(self.m.isa(), *p);
                (numeric::ISA_SUPPORTED, vec![u64::from(enabled)])
            }
            VectorOp::Lanes {
                intrinsic,
                shape,
                constants,
            } => {
                let Some(code) = request_intrinsic(*intrinsic) else {
                    return Ok(None);
                };
                let mut words = vec![
                    code,
                    request_elem(shape.elem),
                    u64::from(shape.lanes),
                    constants.len() as u64,
                ];
                words.extend(constants.iter().map(|c| u64::from(*c)));
                words.extend(args.iter().map(|v| codegen_swap::word(*v)));
                (numeric::VECTOR, words)
            }
        };
        match codegen_swap::request(
            Family::Numeric,
            &mut self.b,
            &mut *self.m,
            self.world,
            self.program,
            codegen_swap::Site::NONE,
            request,
            &words,
            None,
        )? {
            Some(results) => codegen_swap::value_of(&results).map(Some),
            None => Ok(None),
        }
    }
    /// The chunks of a converted vector of shape `target`.
    fn convert_chunks(&mut self, conv: Conv, target: Shape, a: &[ir::Value]) -> Vec<ir::Value> {
        let ty = chunk_type(target.elem);
        match conv {
            Conv::ToBits | Conv::FromBits => a
                .iter()
                .map(|x| self.b.ins().bitcast(ty, little(), *x))
                .collect(),
            // Each f32x4 chunk widens to two f64x2 chunks.
            Conv::Widen => a
                .iter()
                .flat_map(|x| [*x, *x].into_iter().zip([false, true]))
                .collect::<Vec<_>>()
                .into_iter()
                .map(|(x, high)| {
                    let x = if high {
                        let bytes = self.b.ins().bitcast(types::I8X16, little(), x);
                        let upper: Vec<u8> = (8..16).chain(8..16).collect();
                        let upper = self.b.func.dfg.immediates.push(ConstantData::from(upper));
                        let moved = self.b.ins().shuffle(bytes, bytes, upper);
                        self.b.ins().bitcast(types::F32X4, little(), moved)
                    } else {
                        x
                    };
                    self.b.ins().fvpromote_low(x)
                })
                .collect(),
            // Two f64x2 chunks narrow to one f32x4 chunk.
            Conv::Narrow => a
                .chunks(2)
                .map(|pair| {
                    let lo = self.b.ins().fvdemote(pair[0]);
                    let hi = self.b.ins().fvdemote(pair[1]);
                    let lo = self.b.ins().bitcast(types::I8X16, little(), lo);
                    let hi = self.b.ins().bitcast(types::I8X16, little(), hi);
                    let joined: Vec<u8> = (0..8).chain(16..24).collect();
                    let joined = self.b.func.dfg.immediates.push(ConstantData::from(joined));
                    let both = self.b.ins().shuffle(lo, hi, joined);
                    self.b.ins().bitcast(types::F32X4, little(), both)
                })
                .collect(),
        }
    }
    /// `Vec::load`, `Vec::store` and their masked forms on the elements
    /// from `base` (the first element of the view) of a view of `len`
    /// elements packed at `stride` bytes.
    pub(super) fn vector_slice_io(
        &mut self,
        io: SliceIo,
        base: ir::Value,
        len: ir::Value,
        at: ir::Value,
        value: Option<ir::Value>,
        stride: i64,
    ) -> Result<ir::Value, String> {
        let shape = io.shape;
        let lanes = i64::from(shape.lanes);
        let available = self.available(at, len);
        let full = self
            .b
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, available, lanes);
        if !io.masked {
            // One check for the whole interval `at..at + N`.
            self.seq_guard(full)?;
        }
        let offset = self.b.ins().imul_imm(at, stride);
        let start = self.b.ins().iadd(base, offset);
        // A masked load's lanes past the end stay zero; an unmasked load
        // writes every byte.
        let result = match (io.store, io.masked) {
            (true, _) => None,
            (false, true) => Some(self.alloc(shape.bytes())),
            (false, false) => Some(self.alloc_uninit(shape.bytes())),
        };
        // A masked access takes the whole-vector path when every lane is
        // inside the view.
        let (slow, join) = if io.masked {
            let fast = self.b.create_block();
            let slow = self.b.create_block();
            let join = self.b.create_block();
            self.branch(full, fast, &[], slow, &[]);
            self.b.switch_to_block(fast);
            (Some(slow), Some(join))
        } else {
            (None, None)
        };
        // The whole vector: chunk by chunk.
        match (io.store, value) {
            (true, Some(v)) => self.store_lanes_at(shape, v, start, stride),
            _ => {
                let r = result.ok_or("native vector load lacks its result")?;
                self.load_lanes_at(shape, start, stride, r);
            }
        }
        if let (Some(slow), Some(join)) = (slow, join) {
            self.jump(join, &[]);
            // The lanes inside the view, one at a time; a load's other
            // lanes stay zero.
            self.b.switch_to_block(slow);
            let width = i64::from(shape.elem.bits() / 8);
            let lane_ty = lane_type(shape.elem);
            let head = self.b.create_block();
            let body = self.b.create_block();
            self.b.append_block_param(head, types::I64);
            let zero = self.zero();
            self.jump(head, &[zero.into()]);
            self.b.switch_to_block(head);
            let k = self.b.block_params(head)[0];
            let more = self.b.ins().icmp(IntCC::UnsignedLessThan, k, available);
            self.branch(more, body, &[], join, &[]);
            self.b.switch_to_block(body);
            let element = self.b.ins().imul_imm(k, stride);
            let element = self.b.ins().iadd(start, element);
            let lane = self.b.ins().imul_imm(k, width);
            let vector = match (io.store, value) {
                (true, Some(v)) => v,
                _ => result.ok_or("native vector load lacks its result")?,
            };
            let lane = self.b.ins().iadd(vector, lane);
            let (from, to) = if io.store {
                (lane, element)
            } else {
                (element, lane)
            };
            let x = self.b.ins().load(lane_ty, MemFlags::new(), from, 0);
            self.b.ins().store(MemFlags::new(), x, to, 0);
            let next = self.b.ins().iadd_imm(k, 1);
            self.jump(head, &[next.into()]);
            self.b.switch_to_block(join);
        }
        Ok(match result {
            Some(r) => r,
            None => self.zero(),
        })
    }
    /// Loads every lane of `shape` from elements at `start` into the
    /// vector buffer `out`.
    fn load_lanes_at(&mut self, shape: Shape, start: ir::Value, stride: i64, out: ir::Value) {
        let width = i64::from(shape.elem.bits() / 8);
        for c in 0..shape.chunks() {
            let chunk = if stride == width {
                self.b
                    .ins()
                    .load(types::I8X16, little(), start, (c * 16) as i32)
            } else {
                // f32 lanes in eight-byte elements: the low halves of two
                // chunks of elements.
                let at = (c * 32) as i32;
                let lo = self.b.ins().load(types::I8X16, little(), start, at);
                let hi = self.b.ins().load(types::I8X16, little(), start, at + 16);
                let low_halves: Vec<u8> = (0..4u8)
                    .flat_map(|j| (0..4u8).map(move |b| j * 8 + b))
                    .collect();
                let low_halves = self
                    .b
                    .func
                    .dfg
                    .immediates
                    .push(ConstantData::from(low_halves));
                self.b.ins().shuffle(lo, hi, low_halves)
            };
            self.b.ins().store(little(), chunk, out, (c * 16) as i32);
        }
    }
    /// Stores every lane of the vector at `v` to elements at `start`.
    fn store_lanes_at(&mut self, shape: Shape, v: ir::Value, start: ir::Value, stride: i64) {
        let width = i64::from(shape.elem.bits() / 8);
        for c in 0..shape.chunks() {
            let chunk = self
                .b
                .ins()
                .load(types::I8X16, little(), v, (c * 16) as i32);
            if stride == width {
                self.b.ins().store(little(), chunk, start, (c * 16) as i32);
            } else {
                // Spread the four f32 lanes over four eight-byte elements,
                // zeroing their high halves.
                let zero = self.zero_chunk(types::I8X16);
                for half in 0..2u8 {
                    let spread: Vec<u8> = (0..2u8)
                        .flat_map(|j| {
                            let lane = half * 2 + j;
                            (0..4u8).map(move |b| lane * 4 + b).chain([16; 4])
                        })
                        .collect();
                    let spread = self.b.func.dfg.immediates.push(ConstantData::from(spread));
                    let words = self.b.ins().shuffle(chunk, zero, spread);
                    let at = (c * 32) as i32 + i32::from(half) * 16;
                    self.b.ins().store(little(), words, start, at);
                }
            }
        }
    }
    fn zero_chunk(&mut self, ty: ir::Type) -> ir::Value {
        let zero = self
            .b
            .func
            .dfg
            .constants
            .insert(ConstantData::from(vec![0u8; 16]));
        self.b.ins().vconst(ty, zero)
    }
    fn load_chunks(&mut self, ptr: ir::Value, shape: Shape, mask: bool) -> Vec<ir::Value> {
        let ty = if mask {
            mask_type(shape.elem)
        } else {
            chunk_type(shape.elem)
        };
        (0..shape.chunks())
            .map(|c| self.b.ins().load(ty, little(), ptr, (c * 16) as i32))
            .collect()
    }
    fn store_chunks(&mut self, shape: Shape, chunks: &[ir::Value]) -> ir::Value {
        // The chunks write every byte of the vector.
        let ptr = self.alloc_uninit(shape.bytes());
        for (c, chunk) in chunks.iter().enumerate() {
            self.b.ins().store(little(), *chunk, ptr, (c * 16) as i32);
        }
        ptr
    }
    /// A backend scalar (`i64` or `f64`) as one lane.
    fn lane_of(&mut self, v: ir::Value, elem: Elem) -> ir::Value {
        match lane_type(elem) {
            t @ (types::I8 | types::I32) => self.b.ins().ireduce(t, v),
            _ => v,
        }
    }
    /// One lane as a backend scalar.
    fn scalar_of(&mut self, v: ir::Value, elem: Elem) -> ir::Value {
        match lane_type(elem) {
            types::I8 | types::I32 => self.b.ins().uextend(types::I64, v),
            _ => v,
        }
    }
    fn binary_chunk(&mut self, op: BinOp, elem: Elem, x: ir::Value, y: ir::Value) -> ir::Value {
        let ins = self.b.ins();
        if elem.is_float() {
            return match op {
                BinOp::Add => ins.fadd(x, y),
                BinOp::Sub => ins.fsub(x, y),
                BinOp::Mul => ins.fmul(x, y),
                BinOp::Div => ins.fdiv(x, y),
                BinOp::Min => ins.fmin(x, y),
                BinOp::Max => ins.fmax(x, y),
                // Rejected by the checker.
                _ => x,
            };
        }
        let less = if elem.is_signed() {
            IntCC::SignedLessThan
        } else {
            IntCC::UnsignedLessThan
        };
        match op {
            BinOp::Add => ins.iadd(x, y),
            BinOp::Sub => ins.isub(x, y),
            BinOp::Mul => ins.imul(x, y),
            BinOp::Min => {
                let m = ins.icmp(less, y, x);
                self.b.ins().bitselect(m, y, x)
            }
            BinOp::Max => {
                let m = ins.icmp(less, x, y);
                self.b.ins().bitselect(m, y, x)
            }
            BinOp::And => ins.band(x, y),
            BinOp::Or => ins.bor(x, y),
            BinOp::Xor => ins.bxor(x, y),
            BinOp::SaturatingAdd => ins.uadd_sat(x, y),
            BinOp::SaturatingSub => ins.usub_sat(x, y),
            // Rejected by the checker.
            BinOp::Div => x,
        }
    }
    fn reduce_step(&mut self, op: ReduceOp, elem: Elem, a: ir::Value, x: ir::Value) -> ir::Value {
        if elem.is_float() {
            return match op {
                ReduceOp::Min => self.b.ins().fmin(a, x),
                ReduceOp::Max => self.b.ins().fmax(a, x),
                _ => self.b.ins().fadd(a, x),
            };
        }
        let v = match op {
            ReduceOp::Add => self.b.ins().iadd(a, x),
            ReduceOp::Xor => self.b.ins().bxor(a, x),
            ReduceOp::Or => self.b.ins().bor(a, x),
            ReduceOp::And => self.b.ins().band(a, x),
            ReduceOp::Min | ReduceOp::Max => {
                let less = if elem.is_signed() {
                    IntCC::SignedLessThan
                } else {
                    IntCC::UnsignedLessThan
                };
                let (l, r) = if op == ReduceOp::Min { (x, a) } else { (a, x) };
                let take_x = self.b.ins().icmp(less, l, r);
                self.b.ins().select(take_x, x, a)
            }
        };
        if elem.bits() < 64 {
            self.b.ins().band_imm(v, elem.mask() as i64)
        } else {
            v
        }
    }
    /// Lane `i` of the mask at `ptr` as bit `i` of an `i64`.
    fn bitmask(&mut self, ptr: ir::Value, shape: Shape) -> ir::Value {
        let chunks = self.load_chunks(ptr, shape, true);
        let per = i64::from(shape.lanes_per_chunk());
        let mut acc = self.b.ins().iconst(types::I64, 0);
        for (c, chunk) in chunks.into_iter().enumerate() {
            let bits = self.b.ins().vhigh_bits(types::I64, chunk);
            let shifted = if c == 0 {
                bits
            } else {
                self.b.ins().ishl_imm(bits, per * c as i64)
            };
            acc = self.b.ins().bor(acc, shifted);
        }
        acc
    }
    /// Lanes available from `offset` below `len`: `len - offset`, or 0.
    fn available(&mut self, offset: ir::Value, len: ir::Value) -> ir::Value {
        let inside = self.b.ins().icmp(IntCC::UnsignedLessThan, offset, len);
        let rest = self.b.ins().isub(len, offset);
        let zero = self.b.ins().iconst(types::I64, 0);
        self.b.ins().select(inside, rest, zero)
    }
    /// The mask of lanes `i` with `offset + i < len`.
    fn active(&mut self, offset: ir::Value, len: ir::Value, shape: Shape) -> ir::Value {
        let elem = shape.elem;
        let available = self.available(offset, len);
        // Clamp to the lane count so the bound fits every lane type.
        let n = self.b.ins().iconst(types::I64, i64::from(shape.lanes));
        let small = self.b.ins().icmp(IntCC::UnsignedLessThan, available, n);
        let bound = self.b.ins().select(small, available, n);
        let ty = mask_type(elem);
        let lane = match ty.lane_type() {
            t @ (types::I8 | types::I32) => self.b.ins().ireduce(t, bound),
            _ => bound,
        };
        let bounds = self.b.ins().splat(ty, lane);
        let width = (ty.lane_bits() / 8) as usize;
        let per = usize::from(shape.lanes_per_chunk());
        let chunks = (0..shape.chunks())
            .map(|c| {
                let mut bytes = Vec::with_capacity(16);
                for j in 0..per {
                    let i = (c * per + j) as u64;
                    bytes.extend_from_slice(&i.to_le_bytes()[..width]);
                }
                let iota = self.b.func.dfg.constants.insert(ConstantData::from(bytes));
                let iota = self.b.ins().vconst(ty, iota);
                self.b.ins().icmp(IntCC::UnsignedGreaterThan, bounds, iota)
            })
            .collect::<Vec<_>>();
        self.store_chunks(shape, &chunks)
    }
    /// `Vec::load_masked[u8, N](s, offset)`: a direct vector load when all
    /// `N` bytes are inside the string, else lane by lane with zeros past
    /// its end. A string is `{len, cap, data}` (§13 strings), and the fast
    /// path reads whole chunks only when every byte is inside `data`.
    fn load_masked(
        &mut self,
        string: ir::Value,
        offset: ir::Value,
        shape: Shape,
    ) -> Result<ir::Value, String> {
        let result = self.alloc(shape.bytes());
        let len = self.b.ins().load(types::I64, MemFlags::new(), string, 0);
        let available = self.available(offset, len);
        let full = self.b.ins().icmp_imm(
            IntCC::UnsignedGreaterThanOrEqual,
            available,
            i64::from(shape.lanes),
        );
        let base = self.b.ins().load(types::I64, MemFlags::new(), string, 16);
        let fast = self.b.create_block();
        let slow = self.b.create_block();
        let join = self.b.create_block();
        self.branch(full, fast, &[], slow, &[]);
        self.b.switch_to_block(fast);
        let start = self.b.ins().iadd(base, offset);
        for c in 0..shape.chunks() {
            let chunk = self
                .b
                .ins()
                .load(types::I8X16, little(), start, (c * 16) as i32);
            self.b.ins().store(little(), chunk, result, (c * 16) as i32);
        }
        self.jump(join, &[]);
        self.b.switch_to_block(slow);
        let zero = self.b.ins().iconst(types::I64, 0);
        for i in 0..i64::from(shape.lanes) {
            let valid = self
                .b
                .ins()
                .icmp_imm(IntCC::UnsignedGreaterThan, available, i);
            let at = self.b.ins().iadd_imm(offset, i);
            // A lane past the end reads the (zeroed) result buffer instead:
            // the data of an empty string is null.
            let address = self.b.ins().iadd(base, at);
            let address = self.b.ins().select(valid, address, result);
            let byte = self.b.ins().uload8(types::I64, MemFlags::new(), address, 0);
            let byte = self.b.ins().select(valid, byte, zero);
            let byte = self.b.ins().ireduce(types::I8, byte);
            self.b.ins().store(MemFlags::new(), byte, result, i as i32);
        }
        self.jump(join, &[]);
        self.b.switch_to_block(join);
        Ok(result)
    }
}

/// Vector temporaries in registers (#1500).
///
/// A vector is an aggregate in a stack slot, so every operation reads its
/// operands from memory and writes its result to a fresh slot. The CLIF
/// clean-up's mem2reg ([`crate::native_clif_opt::promote_slots`]) turns a
/// slot into SSA values when each offset is accessed with one type, and
/// two things kept vector slots from qualifying: an operation loads a chunk
/// at the lane type (`i32x4`, `f32x4`) where a slice load stored bytes
/// (`i8x16`), and an aggregate copy (a `let` of a vector, an argument or a
/// result) moves the vector as 8-byte words. This pass, run on the
/// optimized path just before the clean-up, fixes both on the slots that
/// hold vectors: a copy's pair of word moves at a 16-byte boundary becomes
/// one `i8x16` move, so does a pair of zero words, and every 128-bit access
/// is made at `i8x16` with a register `bitcast` to the lane type. The
/// clean-up then promotes the slot, and Cranelift spills a vector only
/// where its register allocator decides to.
pub(crate) fn registers(func: &mut ir::Function) {
    // Fusing a copy into a vector slot can make the other end a vector
    // slot, which may enable another copy: a short chain of `let`s.
    for _ in 0..4 {
        if !fuse_words(func) {
            break;
        }
    }
    retype_vector_accesses(func);
}

/// Stack-slot addresses: value -> (slot, offset), and the slots whose
/// address is used other than as the base of a load or store.
fn slot_addresses(
    func: &ir::Function,
) -> (
    std::collections::HashMap<ir::Value, (ir::StackSlot, i64)>,
    std::collections::HashSet<ir::StackSlot>,
) {
    use ir::{InstructionData as D, Opcode};
    let mut addr = std::collections::HashMap::new();
    let mut escaped = std::collections::HashSet::new();
    let insts: Vec<ir::Inst> = func
        .layout
        .blocks()
        .flat_map(|b| func.layout.block_insts(b))
        .collect();
    loop {
        let before = addr.len();
        for &inst in &insts {
            match func.dfg.insts[inst] {
                D::StackLoad {
                    opcode: Opcode::StackAddr,
                    stack_slot,
                    offset,
                } => {
                    addr.insert(func.dfg.first_result(inst), (stack_slot, i64::from(offset)));
                }
                D::BinaryImm64 {
                    opcode: Opcode::IaddImm,
                    arg,
                    imm,
                } => {
                    if let Some(&(slot, off)) = addr.get(&func.dfg.resolve_aliases(arg)) {
                        addr.insert(
                            func.dfg.first_result(inst),
                            (slot, off.wrapping_add(imm.bits())),
                        );
                    }
                }
                _ => {}
            }
        }
        if addr.len() == before {
            break;
        }
    }
    for &inst in &insts {
        match func.dfg.insts[inst] {
            D::StackLoad {
                opcode: Opcode::StackAddr,
                ..
            } => continue,
            D::BinaryImm64 {
                opcode: Opcode::IaddImm,
                arg,
                ..
            } if addr.contains_key(&func.dfg.resolve_aliases(arg)) => continue,
            D::StackLoad { stack_slot, .. } | D::StackStore { stack_slot, .. } => {
                escaped.insert(stack_slot);
                continue;
            }
            D::Load {
                opcode: Opcode::Load,
                ..
            } => continue,
            D::Store {
                opcode: Opcode::Store,
                args,
                ..
            } => {
                // A slot address stored as a value escapes.
                let value = func.dfg.resolve_aliases(args[0]);
                if let Some(&(slot, _)) = addr.get(&value) {
                    escaped.insert(slot);
                }
                continue;
            }
            _ => {}
        }
        for v in func.dfg.inst_values(inst) {
            if let Some(&(slot, _)) = addr.get(&func.dfg.resolve_aliases(v)) {
                escaped.insert(slot);
            }
        }
    }
    (addr, escaped)
}

/// A plain load or store: (base, offset, type, stored value).
fn access(
    func: &ir::Function,
    inst: ir::Inst,
) -> Option<(ir::Value, i64, ir::Type, Option<ir::Value>)> {
    use ir::{InstructionData as D, Opcode};
    match func.dfg.insts[inst] {
        D::Load {
            opcode: Opcode::Load,
            arg,
            offset,
            ..
        } => Some((
            func.dfg.resolve_aliases(arg),
            i64::from(i32::from(offset)),
            func.dfg.value_type(func.dfg.first_result(inst)),
            None,
        )),
        D::Store {
            opcode: Opcode::Store,
            args,
            offset,
            ..
        } => {
            let value = func.dfg.resolve_aliases(args[0]);
            Some((
                func.dfg.resolve_aliases(args[1]),
                i64::from(i32::from(offset)),
                func.dfg.value_type(value),
                Some(value),
            ))
        }
        _ => None,
    }
}

fn is_chunk(ty: ir::Type) -> bool {
    ty.is_vector() && ty.bits() == 128
}

/// The slots accessed with a 128-bit vector at a 16-byte boundary.
fn vector_slots(
    func: &ir::Function,
    addr: &std::collections::HashMap<ir::Value, (ir::StackSlot, i64)>,
    escaped: &std::collections::HashSet<ir::StackSlot>,
) -> std::collections::HashSet<ir::StackSlot> {
    let mut out = std::collections::HashSet::new();
    for block in func.layout.blocks() {
        for inst in func.layout.block_insts(block) {
            if let Some((base, offset, ty, _)) = access(func, inst)
                && is_chunk(ty)
                && let Some(&(slot, off)) = addr.get(&base)
                && (off + offset) % 16 == 0
                && !escaped.contains(&slot)
            {
                out.insert(slot);
            }
        }
    }
    out
}

/// Rewrites word pairs that move or zero 16 bytes of a vector slot into
/// one `i8x16` access. Returns whether anything changed.
fn fuse_words(func: &mut ir::Function) -> bool {
    use cranelift_codegen::cursor::{Cursor, FuncCursor};
    let (addr, escaped) = slot_addresses(func);
    let vectors = vector_slots(func, &addr, &escaped);
    if vectors.is_empty() {
        return false;
    }
    // Uses of each value, to know a copied word has no other reader.
    let mut uses: std::collections::HashMap<ir::Value, u32> = std::collections::HashMap::new();
    for block in func.layout.blocks() {
        for inst in func.layout.block_insts(block) {
            for v in func.dfg.inst_values(inst) {
                *uses.entry(func.dfg.resolve_aliases(v)).or_default() += 1;
            }
        }
    }
    // Whether `base + offset` is a 16-byte boundary of a vector slot, or
    // (for the other end of a copy) not a slot this pass would pessimize.
    let vector_at = |base: ir::Value, offset: i64| {
        addr.get(&base)
            .is_some_and(|&(slot, off)| vectors.contains(&slot) && (off + offset) % 16 == 0)
    };
    let other_ok = |base: ir::Value| {
        addr.get(&base)
            .is_none_or(|&(slot, _)| !escaped.contains(&slot))
    };
    let zero_word = |func: &ir::Function, v: ir::Value| {
        matches!(
            func.dfg.value_def(v).inst().map(|i| &func.dfg.insts[i]),
            Some(ir::InstructionData::UnaryImm {
                opcode: ir::Opcode::Iconst,
                imm,
            }) if imm.bits() == 0
        )
    };
    let mut changed = false;
    let blocks: Vec<ir::Block> = func.layout.blocks().collect();
    for block in blocks {
        let body: Vec<ir::Inst> = func.layout.block_insts(block).collect();
        let mut i = 0;
        while i + 1 < body.len() {
            // Two zero words: `store 0, p+o; store 0, p+o+8`.
            if let (Some((b0, o0, types::I64, Some(z0))), Some((b1, o1, types::I64, Some(z1)))) =
                (access(func, body[i]), access(func, body[i + 1]))
                && b0 == b1
                && o1 == o0 + 8
                && vector_at(b0, o0)
                && zero_word(func, z0)
                && zero_word(func, z1)
            {
                let mut pos = FuncCursor::new(func).at_inst(body[i]);
                let zero = pos
                    .func
                    .dfg
                    .constants
                    .insert(ConstantData::from(vec![0u8; 16]));
                let chunk = pos.ins().vconst(types::I8X16, zero);
                pos.ins().store(little(), chunk, b0, o0 as i32);
                pos.func.layout.remove_inst(body[i]);
                pos.func.layout.remove_inst(body[i + 1]);
                changed = true;
                i += 2;
                continue;
            }
            // A copy: `w0 = load p+o; store w0, q+r; w1 = load p+o+8;
            // store w1, q+r+8`, each word read only by its store.
            if i + 3 < body.len()
                && let Some((src, so, types::I64, None)) = access(func, body[i])
                && let Some((dst, d0, types::I64, Some(w0))) = access(func, body[i + 1])
                && let Some((src1, so1, types::I64, None)) = access(func, body[i + 2])
                && let Some((dst1, d1, types::I64, Some(w1))) = access(func, body[i + 3])
                && src1 == src
                && dst1 == dst
                && so1 == so + 8
                && d1 == d0 + 8
                && w0 == func.dfg.first_result(body[i])
                && w1 == func.dfg.first_result(body[i + 2])
                && uses.get(&w0) == Some(&1)
                && uses.get(&w1) == Some(&1)
                && ((vector_at(src, so) && other_ok(dst)) || (vector_at(dst, d0) && other_ok(src)))
            {
                let mut pos = FuncCursor::new(func).at_inst(body[i]);
                let chunk = pos.ins().load(types::I8X16, little(), src, so as i32);
                pos.ins().store(little(), chunk, dst, d0 as i32);
                for &inst in &body[i..i + 4] {
                    pos.func.layout.remove_inst(inst);
                }
                changed = true;
                i += 4;
                continue;
            }
            i += 1;
        }
    }
    changed
}

/// Every 128-bit access of a vector slot at `i8x16`, so each offset has one
/// type; the lane type is a register `bitcast` away.
fn retype_vector_accesses(func: &mut ir::Function) {
    use cranelift_codegen::cursor::{Cursor, FuncCursor};
    let (addr, escaped) = slot_addresses(func);
    let vectors = vector_slots(func, &addr, &escaped);
    if vectors.is_empty() {
        return;
    }
    let blocks: Vec<ir::Block> = func.layout.blocks().collect();
    for block in blocks {
        let body: Vec<ir::Inst> = func.layout.block_insts(block).collect();
        for inst in body {
            let Some((base, offset, ty, value)) = access(func, inst) else {
                continue;
            };
            if !is_chunk(ty)
                || ty == types::I8X16
                || !addr
                    .get(&base)
                    .is_some_and(|(slot, off)| vectors.contains(slot) && (off + offset) % 16 == 0)
            {
                continue;
            }
            let mut pos = FuncCursor::new(func).at_inst(inst);
            match value {
                None => {
                    let raw = pos.ins().load(types::I8X16, little(), base, offset as i32);
                    pos.func.dfg.replace(inst).bitcast(ty, little(), raw);
                }
                Some(v) => {
                    let raw = pos.ins().bitcast(types::I8X16, little(), v);
                    pos.func
                        .dfg
                        .replace(inst)
                        .store(little(), raw, base, offset as i32);
                }
            }
        }
    }
}
