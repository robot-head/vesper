//! Cranelift SIMD lowering of the Place-IR vector operations (§14.3, lane
//! 28). A child module of `native`.
//!
//! A vector or mask is an aggregate: a pointer to its packed lane bytes
//! (see [`crate::vector::layout_words`]). Operations load it as one, two or
//! four 128-bit Cranelift vectors (`i8x16`, `i32x4`, `i64x2`, `f64x2`),
//! compute chunk by chunk and store the result into a fresh stack slot.
//! Cranelift 0.125 has no 256- or 512-bit vector types, so wider shapes are
//! chunked 128-bit code under every profile. Mask lanes are all ones or all
//! zeros in the lane width. Scalars follow the backend's convention: integers
//! and bools are zero-extended `i64`, floats are `f64`.
use super::Codegen;
use crate::vector::{BinOp, CmpOp, Elem, Intrinsic, MaskOp, ReduceOp, Shape, ShiftOp, VectorOp};
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
        Elem::F64 => types::F64X2,
    }
}
/// The integer chunk type masks of `elem` use.
fn mask_type(elem: Elem) -> ir::Type {
    match elem {
        Elem::F64 => types::I64X2,
        e => chunk_type(e),
    }
}
fn lane_type(elem: Elem) -> ir::Type {
    match elem {
        Elem::U8 => types::I8,
        Elem::U32 => types::I32,
        Elem::U64 | Elem::I64 => types::I64,
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

impl<M: Module> Codegen<'_, '_, M> {
    /// Lower one vector operation on its IR operands.
    pub(super) fn vector_op(
        &mut self,
        op: &VectorOp,
        args: &[ir::Value],
    ) -> Result<ir::Value, String> {
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
                            self.b.ins().bitcast(types::F64X2, little(), m[c])
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
        })
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
        let ptr = self.alloc(shape.bytes());
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
    /// its end. Inline strings are a length word followed by the bytes, and
    /// every in-range byte lies inside the module's string capacity.
    fn load_masked(
        &mut self,
        string: ir::Value,
        offset: ir::Value,
        shape: Shape,
    ) -> Result<ir::Value, String> {
        let result = self.alloc(shape.bytes());
        if self.world.string_capacity == 0 {
            // No string has a byte: every lane is zero.
            return Ok(result);
        }
        let len = self.b.ins().load(types::I64, MemFlags::new(), string, 0);
        let available = self.available(offset, len);
        let full = self.b.ins().icmp_imm(
            IntCC::UnsignedGreaterThanOrEqual,
            available,
            i64::from(shape.lanes),
        );
        let base = self.b.ins().iadd_imm(string, 8);
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
            let at = self.b.ins().select(valid, at, zero);
            let address = self.b.ins().iadd(base, at);
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
