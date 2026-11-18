//! Cranelift lowering of the block and fixed-width sequence builtins (§13):
//! `copy_from` and `try_copy_from` (one bounds check, then `memmove`),
//! `fill` (`memset` for bytes, a store loop otherwise) and the `read_*` /
//! `write_*` integer accesses of byte sequences (one bounds check, then one
//! unaligned load or store with a byte swap for big-endian), and the byte
//! scans `sum_be16`, `eq` and `find_byte` (Stage 1 performance lane 6,
//! #936) and `find_either`, `find_not_in`, `eq_ignore_ascii_case` and
//! `extend_ascii_lowercase` (port gaps lane 4, #987): optimized builds for
//! x86-64 and AArch64 run them on 128-bit Cranelift vectors, 16 bytes per
//! step, with a scalar loop for the tail; unoptimized builds and other
//! targets run the scalar loop alone. `find_not_in` looks its class up with
//! a byte shuffle (`swizzle`), so on x86-64 it is vectorized only where the
//! ISA has SSSE3 (`pshufb`); the baseline x86-64 target runs it scalar. Every
//! path gives the same result for every input, and so does the evaluator
//! (`src/seq/eval.rs`), which is the reference semantics.
use super::*;

/// Bytes per vector step.
const LANES: i64 = 16;
/// Vector steps one `sum_be16` block adds in 16-bit lanes before it widens
/// them: 256 steps add at most `256 * 255 = 65_280` per lane.
const SUM_BLOCK_STEPS: i64 = 256;

fn little() -> MemFlags {
    MemFlags::new().with_endianness(ir::Endianness::Little)
}

/// What a first-byte search looks for (`find_byte`, `find_either`,
/// `find_not_in`). Bytes are zero-extended `i64` values.
#[derive(Clone, Copy)]
enum Needle {
    /// A byte equal to the operand.
    One(ir::Value),
    /// A byte equal to either operand.
    Either(ir::Value, ir::Value),
    /// A byte outside the class `{lo, hi, high}` (see [`SeqOp::FindNotIn`]).
    NotIn {
        lo: ir::Value,
        hi: ir::Value,
        high: ir::Value,
    },
}

/// The loop-invariant vectors of a [`Needle`].
#[derive(Clone, Copy)]
enum Splat {
    One(ir::Value),
    Either(ir::Value, ir::Value),
    NotIn {
        /// Byte `j` holds the class bits of bytes `8j..8j + 8`.
        table: ir::Value,
        /// `1 << (k & 7)` in lane `k`.
        bits: ir::Value,
        sevens: ir::Value,
        zeros: ir::Value,
        /// All ones when bytes >= 128 are in the class, else zero.
        high: ir::Value,
    },
}

impl<M: Module> Codegen<'_, '_, M> {
    /// Lowers `op` when it is a block or fixed-width builtin, else `None`.
    pub(super) fn byte_seq_op(
        &mut self,
        op: SeqOp,
        args: &[ir::Value],
        arg_types: &[Type],
    ) -> Result<Option<ir::Value>, String> {
        if op.byte_scan() {
            return self.byte_scan_op(op, args, arg_types).map(Some);
        }
        if !matches!(op, SeqOp::CopyFrom { .. } | SeqOp::Fill | SeqOp::Wide(_)) {
            return Ok(None);
        }
        let arg = |i: usize| {
            args.get(i)
                .copied()
                .ok_or_else(|| format!("native ChunkSeq::{} lacks an operand", op.name()))
        };
        let ty = arg_types
            .first()
            .ok_or_else(|| format!("native ChunkSeq::{} lacks its receiver", op.name()))?;
        let elem = seq::elem(ty)
            .cloned()
            .ok_or_else(|| format!("native ChunkSeq::{} lacks its element type", op.name()))?;
        let elem = crate::numeric::default_type(&elem);
        let stride = self.seq_stride(&elem)?;
        let (h, start, len) = self.seq_view(arg(0)?, ty);
        Ok(Some(match op {
            SeqOp::CopyFrom { total } => {
                let at = arg(1)?;
                let source = arg_types
                    .get(2)
                    .ok_or("native copy_from lacks its source")?;
                let (sh, sstart, n) = self.seq_view(arg(2)?, source);
                let valid = self.seq_fits(len, at, n);
                self.seq_when(valid, total, |this| {
                    let dst = this.seq_elem_addr(h, start, at, stride);
                    let zero = this.zero();
                    let src = this.seq_elem_addr(sh, sstart, zero, stride);
                    let bytes = this.b.ins().imul_imm(n, stride);
                    this.seq_move(dst, src, bytes);
                    Ok(())
                })?
            }
            SeqOp::Fill => {
                let value = arg(1)?;
                let scalar = !Layout::of(self.world, &elem)?.aggregate();
                if stride == 1 && scalar && self.seq_has_libc() {
                    let zero = self.zero();
                    let dst = self.seq_elem_addr(h, start, zero, 1);
                    let byte = self.coerce(value, Some(&elem));
                    let byte = self.b.ins().ireduce(types::I8, byte);
                    let config = self.m.target_config();
                    self.b.call_memset(config, dst, byte, len);
                } else {
                    let head = self.b.create_block();
                    let body = self.b.create_block();
                    let done = self.b.create_block();
                    self.b.append_block_param(head, types::I64);
                    let zero = self.zero();
                    self.jump(head, &[zero.into()]);
                    self.b.switch_to_block(head);
                    let k = self.b.block_params(head)[0];
                    let more = self.b.ins().icmp(IntCC::UnsignedLessThan, k, len);
                    self.branch(more, body, &[], done, &[]);
                    self.b.switch_to_block(body);
                    let addr = self.seq_elem_addr(h, start, k, stride);
                    self.seq_store_elem(addr, value, &elem)?;
                    let next = self.b.ins().iadd_imm(k, 1);
                    self.jump(head, &[next.into()]);
                    self.b.switch_to_block(done);
                }
                self.zero()
            }
            SeqOp::Wide(wide) => {
                let at = arg(1)?;
                let width = self.b.ins().iconst(types::I64, i64::from(wide.bytes));
                let valid = self.seq_fits(len, at, width);
                let addr = self.seq_elem_addr(h, start, at, 1);
                let int = match wide.bytes {
                    2 => types::I16,
                    4 => types::I32,
                    _ => types::I64,
                };
                if wide.write {
                    let value = arg(2)?;
                    self.seq_when(valid, wide.total, |this| {
                        let mut v = value;
                        if int != types::I64 {
                            v = this.b.ins().ireduce(int, v);
                        }
                        if wide.big {
                            v = this.b.ins().bswap(v);
                        }
                        this.b.ins().store(MemFlags::new(), v, addr, 0);
                        Ok(())
                    })?
                } else {
                    let load = move |this: &mut Self| {
                        let mut v = this.b.ins().load(int, MemFlags::new(), addr, 0);
                        if wide.big {
                            v = this.b.ins().bswap(v);
                        }
                        if int != types::I64 {
                            v = this.b.ins().uextend(types::I64, v);
                        }
                        Ok(v)
                    };
                    if wide.total {
                        let int_ty = Type::named(wide.int_type());
                        let option = Type::Named("Option".into(), vec![int_ty.clone()]);
                        self.seq_option(&option, &int_ty, valid, load)?
                    } else {
                        self.seq_guard(valid)?;
                        load(self)?
                    }
                }
            }
            _ => unreachable!("matched above"),
        }))
    }

    /// Whether `n` elements from `at` fit in `len`, without overflow.
    fn seq_fits(&mut self, len: ir::Value, at: ir::Value, n: ir::Value) -> ir::Value {
        let below = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, at, len);
        let room = self.b.ins().isub(len, at);
        let fits = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, n, room);
        self.b.ins().band(below, fits)
    }

    /// Runs `write` when `valid` holds. The checked form raises
    /// `IndexOutOfBounds` otherwise and yields `Unit`; the total form skips
    /// the write and yields `valid` as a `bool`.
    fn seq_when(
        &mut self,
        valid: ir::Value,
        total: bool,
        write: impl FnOnce(&mut Self) -> Result<(), String>,
    ) -> Result<ir::Value, String> {
        if !total {
            self.seq_guard(valid)?;
            write(self)?;
            return Ok(self.zero());
        }
        let yes = self.b.create_block();
        let done = self.b.create_block();
        self.branch(valid, yes, &[], done, &[]);
        self.b.switch_to_block(yes);
        write(self)?;
        self.jump(done, &[]);
        self.b.switch_to_block(done);
        Ok(self.b.ins().uextend(types::I64, valid))
    }

    /// Whether generated code can call the C library (`memmove`, `memset`):
    /// the JIT and every hosted executable, but not a freestanding image.
    fn seq_has_libc(&self) -> bool {
        std::any::type_name::<M>().contains("JITModule")
            || self.m.isa().triple().operating_system != target_lexicon::OperatingSystem::None_
    }

    /// Copies `bytes` bytes from `src` to `dst`, which may overlap.
    pub(super) fn seq_move(&mut self, dst: ir::Value, src: ir::Value, bytes: ir::Value) {
        if self.seq_has_libc() {
            let config = self.m.target_config();
            self.b.call_memmove(config, dst, src, bytes);
        } else {
            // Only `copy_from` and `extend` move, and their checker keeps the
            // source out of the destination, so a forward copy is exact.
            self.seq_copy(dst, src, bytes);
        }
    }

    /// Whether byte scans use 128-bit vectors: optimized builds only (the
    /// Stage 1 corpus is built unoptimized), on x86-64 (SSE2) and AArch64
    /// (NEON), whose baselines have every vector instruction used here.
    fn byte_scans_vectorized(&self) -> bool {
        self.operands_only
            && matches!(
                self.m.isa().triple().architecture,
                target_lexicon::Architecture::X86_64 | target_lexicon::Architecture::Aarch64(_)
            )
    }

    /// Lowers `sum_be16`, `eq` or `find_byte`.
    fn byte_scan_op(
        &mut self,
        op: SeqOp,
        args: &[ir::Value],
        arg_types: &[Type],
    ) -> Result<ir::Value, String> {
        let operand = |i: usize| {
            args.get(i)
                .copied()
                .zip(arg_types.get(i))
                .ok_or_else(|| format!("native ChunkSeq::{} lacks an operand", op.name()))
        };
        let (value, ty) = operand(0)?;
        let (h, start, len) = self.seq_view(value, ty);
        let zero = self.zero();
        let p = self.seq_elem_addr(h, start, zero, 1);
        let simd = self.byte_scans_vectorized();
        Ok(match op {
            SeqOp::SumBe16 => {
                let (acc, from) = if simd {
                    self.sum_be16_vector(p, len)
                } else {
                    (zero, zero)
                };
                self.sum_be16_scalar(p, from, len, acc)
            }
            SeqOp::BytesEq | SeqOp::EqIgnoreAsciiCase => {
                let (value, ty) = operand(1)?;
                let (h2, start2, len2) = self.seq_view(value, ty);
                let q = self.seq_elem_addr(h2, start2, zero, 1);
                self.bytes_eq(p, q, len, len2, simd, op == SeqOp::EqIgnoreAsciiCase)
            }
            SeqOp::FindByte | SeqOp::FindEither | SeqOp::FindNotIn => {
                let byte = |this: &mut Self, i: usize| -> Result<ir::Value, String> {
                    let (v, _) = operand(i)?;
                    Ok(this.b.ins().band_imm(v, 0xff))
                };
                let (needle, simd) = match op {
                    SeqOp::FindByte => (Needle::One(byte(self, 1)?), simd),
                    SeqOp::FindEither => (Needle::Either(byte(self, 1)?, byte(self, 2)?), simd),
                    _ => {
                        let (lo, _) = operand(1)?;
                        let (hi, _) = operand(2)?;
                        let (high, _) = operand(3)?;
                        let high = self.b.ins().band_imm(high, 1);
                        // The class lookup is a byte shuffle: SSSE3 on x86-64.
                        let shuffles = self.m.isa().has_x86_pshufb_lowering()
                            || matches!(
                                self.m.isa().triple().architecture,
                                target_lexicon::Architecture::Aarch64(_)
                            );
                        (Needle::NotIn { lo, hi, high }, simd && shuffles)
                    }
                };
                let (present, index) = self.find_first(p, len, needle, simd);
                let u64_ty = Type::named("u64");
                let option = Type::Named("Option".into(), vec![u64_ty.clone()]);
                self.seq_option(&option, &u64_ty, present, |_| Ok(index))?
            }
            SeqOp::ExtendAsciiLowercase => {
                let (src, src_ty) = operand(1)?;
                let len = self.seq_word(h, LEN);
                let (sh, sstart, slen) = self.seq_view(src, src_ty);
                let needed = self.b.ins().iadd(len, slen);
                self.seq_reserve(h, needed, 1)?;
                // The data pointer may move when the sequence grows.
                let dst = self.seq_elem_addr(h, zero, len, 1);
                let src = self.seq_elem_addr(sh, sstart, zero, 1);
                self.lowercase_copy(dst, src, slen, simd);
                self.seq_set_word(h, LEN, needed);
                zero
            }
            _ => unreachable!("only byte scans reach here"),
        })
    }

    /// Leaves the vector loop of a scan: `more` continues at `body(k)`.
    /// Otherwise, when at least one whole step fits in `len` and bytes are
    /// left, the last step runs once more on the final 16 bytes
    /// (`body(len - 16)`), which overlap bytes the loop has already seen; a
    /// search or compare may see them twice, and the step after it ends at
    /// `len`. Shorter inputs finish in `scalar(k)`.
    fn vector_head_exit(
        &mut self,
        more: ir::Value,
        k: ir::Value,
        len: ir::Value,
        body: ir::Block,
        scalar: ir::Block,
    ) {
        let tail = self.b.create_block();
        self.branch(more, body, &[k.into()], tail, &[]);
        self.b.switch_to_block(tail);
        let whole = self
            .b
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, len, LANES);
        let left = self.b.ins().icmp(IntCC::UnsignedLessThan, k, len);
        let last = self.b.ins().band(whole, left);
        let from = self.b.ins().iadd_imm(len, -LANES);
        self.branch(last, body, &[from.into()], scalar, &[k.into()]);
    }

    /// `len` rounded down to a whole number of vector steps.
    fn vector_end(&mut self, len: ir::Value) -> ir::Value {
        self.b.ins().band_imm(len, -LANES)
    }

    /// The big-endian 16-bit words of `p[from..len]` added to `acc`, a byte
    /// at a time. `from` is even.
    fn sum_be16_scalar(
        &mut self,
        p: ir::Value,
        from: ir::Value,
        len: ir::Value,
        acc: ir::Value,
    ) -> ir::Value {
        let head = self.b.create_block();
        let body = self.b.create_block();
        let pairs_done = self.b.create_block();
        let odd = self.b.create_block();
        let done = self.b.create_block();
        for block in [head, pairs_done] {
            self.b.append_block_param(block, types::I64);
            self.b.append_block_param(block, types::I64);
        }
        self.b.append_block_param(done, types::I64);
        self.jump(head, &[from.into(), acc.into()]);

        self.b.switch_to_block(head);
        let k = self.b.block_params(head)[0];
        let acc = self.b.block_params(head)[1];
        let next = self.b.ins().iadd_imm(k, 1);
        let more = self.b.ins().icmp(IntCC::UnsignedLessThan, next, len);
        self.branch(more, body, &[], pairs_done, &[k.into(), acc.into()]);

        self.b.switch_to_block(body);
        let at = self.b.ins().iadd(p, k);
        let hi = self.b.ins().uload8(types::I64, MemFlags::new(), at, 0);
        let lo = self.b.ins().uload8(types::I64, MemFlags::new(), at, 1);
        let hi = self.b.ins().ishl_imm(hi, 8);
        let word = self.b.ins().bor(hi, lo);
        let sum = self.b.ins().iadd(acc, word);
        let k2 = self.b.ins().iadd_imm(k, 2);
        self.jump(head, &[k2.into(), sum.into()]);

        self.b.switch_to_block(pairs_done);
        let k = self.b.block_params(pairs_done)[0];
        let acc = self.b.block_params(pairs_done)[1];
        let left = self.b.ins().icmp(IntCC::UnsignedLessThan, k, len);
        self.branch(left, odd, &[], done, &[acc.into()]);

        self.b.switch_to_block(odd);
        let at = self.b.ins().iadd(p, k);
        let hi = self.b.ins().uload8(types::I64, MemFlags::new(), at, 0);
        let hi = self.b.ins().ishl_imm(hi, 8);
        let sum = self.b.ins().iadd(acc, hi);
        self.jump(done, &[sum.into()]);

        self.b.switch_to_block(done);
        self.b.block_params(done)[0]
    }

    /// The `sum_be16` of the whole vector steps of `p[..len]`, and where
    /// they end. A step loads eight little-endian 16-bit lanes: the low
    /// byte of each is an even (high-order) byte and the high byte an odd
    /// one, so the sum is `256 * evens + odds`. Each block of
    /// [`SUM_BLOCK_STEPS`] steps adds evens and odds in 16-bit lanes, then
    /// widens them to 64-bit lanes, which cannot overflow before the
    /// scalar sum would wrap.
    fn sum_be16_vector(&mut self, p: ir::Value, len: ir::Value) -> (ir::Value, ir::Value) {
        let end = self.vector_end(len);
        let zero = self.zero();
        let zero16 = self.b.ins().iconst(types::I16, 0);
        let zero16 = self.b.ins().splat(types::I16X8, zero16);
        let zero64 = self.b.ins().splat(types::I64X2, zero);
        let low8 = self.b.ins().iconst(types::I16, 0xff);
        let low8 = self.b.ins().splat(types::I16X8, low8);
        let low16 = self.b.ins().iconst(types::I32, 0xffff);
        let low16 = self.b.ins().splat(types::I32X4, low16);
        let low32 = self.b.ins().iconst(types::I64, 0xffff_ffff);
        let low32 = self.b.ins().splat(types::I64X2, low32);

        let outer = self.b.create_block();
        let block_start = self.b.create_block();
        let inner = self.b.create_block();
        let step = self.b.create_block();
        let widen = self.b.create_block();
        let done = self.b.create_block();
        self.b.append_block_param(outer, types::I64);
        self.b.append_block_param(outer, types::I64X2);
        self.b.append_block_param(inner, types::I64);
        self.b.append_block_param(inner, types::I16X8);
        self.b.append_block_param(inner, types::I16X8);
        self.b.append_block_param(done, types::I64X2);
        self.jump(outer, &[zero.into(), zero64.into()]);

        // outer(k, total): one block of steps from k, or done.
        self.b.switch_to_block(outer);
        let k = self.b.block_params(outer)[0];
        let total = self.b.block_params(outer)[1];
        let more = self.b.ins().icmp(IntCC::UnsignedLessThan, k, end);
        self.branch(more, block_start, &[], done, &[total.into()]);

        self.b.switch_to_block(block_start);
        let room = self.b.ins().isub(end, k);
        let cap = self.b.ins().iconst(types::I64, SUM_BLOCK_STEPS * LANES);
        let short = self.b.ins().icmp(IntCC::UnsignedLessThan, room, cap);
        let span = self.b.ins().select(short, room, cap);
        let block_end = self.b.ins().iadd(k, span);
        self.jump(inner, &[k.into(), zero16.into(), zero16.into()]);

        // inner(i, evens, odds): one vector step, or widen the block.
        self.b.switch_to_block(inner);
        let i = self.b.block_params(inner)[0];
        let evens = self.b.block_params(inner)[1];
        let odds = self.b.block_params(inner)[2];
        let more = self.b.ins().icmp(IntCC::UnsignedLessThan, i, block_end);
        self.branch(more, step, &[], widen, &[]);

        self.b.switch_to_block(step);
        let at = self.b.ins().iadd(p, i);
        let v = self.b.ins().load(types::I16X8, little(), at, 0);
        let even = self.b.ins().band(v, low8);
        let odd = self.b.ins().ushr_imm(v, 8);
        let evens2 = self.b.ins().iadd(evens, even);
        let odds2 = self.b.ins().iadd(odds, odd);
        let i2 = self.b.ins().iadd_imm(i, LANES);
        self.jump(inner, &[i2.into(), evens2.into(), odds2.into()]);

        // Widen: 16-bit lane pairs to 32 bits, weight, 32-bit pairs to 64.
        self.b.switch_to_block(widen);
        let pairs = |this: &mut Self, v: ir::Value| {
            let v = this.b.ins().bitcast(types::I32X4, little(), v);
            let lo = this.b.ins().band(v, low16);
            let hi = this.b.ins().ushr_imm(v, 16);
            this.b.ins().iadd(lo, hi)
        };
        let evens = pairs(self, evens);
        let odds = pairs(self, odds);
        let weighted = self.b.ins().ishl_imm(evens, 8);
        let words = self.b.ins().iadd(weighted, odds);
        let words = self.b.ins().bitcast(types::I64X2, little(), words);
        let lo = self.b.ins().band(words, low32);
        let hi = self.b.ins().ushr_imm(words, 32);
        let total = self.b.ins().iadd(total, lo);
        let total = self.b.ins().iadd(total, hi);
        self.jump(outer, &[block_end.into(), total.into()]);

        self.b.switch_to_block(done);
        let total = self.b.block_params(done)[0];
        let slot = self.alloc(16);
        self.b.ins().store(little(), total, slot, 0);
        let a = self.b.ins().load(types::I64, MemFlags::new(), slot, 0);
        let b = self.b.ins().load(types::I64, MemFlags::new(), slot, 8);
        (self.b.ins().iadd(a, b), end)
    }

    /// Whether `p[..len]` and `q[..len2]` hold the same bytes, as a `bool`.
    fn bytes_eq(
        &mut self,
        p: ir::Value,
        q: ir::Value,
        len: ir::Value,
        len2: ir::Value,
        simd: bool,
        fold: bool,
    ) -> ir::Value {
        let zero = self.zero();
        let scalar = self.b.create_block();
        let scalar_body = self.b.create_block();
        let done = self.b.create_block();
        self.b.append_block_param(scalar, types::I64);
        self.b.append_block_param(done, types::I64);
        let no = self.b.ins().iconst(types::I64, 0);
        let yes = self.b.ins().iconst(types::I64, 1);
        let same_len = self.b.ins().icmp(IntCC::Equal, len, len2);
        let start = self.b.create_block();
        self.branch(same_len, start, &[], done, &[no.into()]);
        self.b.switch_to_block(start);
        if simd {
            let end = self.vector_end(len);
            let head = self.b.create_block();
            let body = self.b.create_block();
            self.b.append_block_param(head, types::I64);
            self.b.append_block_param(body, types::I64);
            self.jump(head, &[zero.into()]);

            self.b.switch_to_block(head);
            let k = self.b.block_params(head)[0];
            let more = self.b.ins().icmp(IntCC::UnsignedLessThan, k, end);
            self.vector_head_exit(more, k, len, body, scalar);

            self.b.switch_to_block(body);
            let k = self.b.block_params(body)[0];
            let a = self.b.ins().iadd(p, k);
            let mut a = self.b.ins().load(types::I8X16, little(), a, 0);
            let b = self.b.ins().iadd(q, k);
            let mut b = self.b.ins().load(types::I8X16, little(), b, 0);
            if fold {
                a = self.lower_vector(a);
                b = self.lower_vector(b);
            }
            let same = self.b.ins().icmp(IntCC::Equal, a, b);
            let bits = self.b.ins().vhigh_bits(types::I32, same);
            let all = self.b.ins().icmp_imm(IntCC::Equal, bits, 0xffff);
            let k2 = self.b.ins().iadd_imm(k, LANES);
            self.branch(all, head, &[k2.into()], done, &[no.into()]);
        } else {
            self.jump(scalar, &[zero.into()]);
        }

        self.b.switch_to_block(scalar);
        let k = self.b.block_params(scalar)[0];
        let more = self.b.ins().icmp(IntCC::UnsignedLessThan, k, len);
        self.branch(more, scalar_body, &[], done, &[yes.into()]);

        self.b.switch_to_block(scalar_body);
        let a = self.b.ins().iadd(p, k);
        let mut a = self.b.ins().uload8(types::I64, MemFlags::new(), a, 0);
        let b = self.b.ins().iadd(q, k);
        let mut b = self.b.ins().uload8(types::I64, MemFlags::new(), b, 0);
        if fold {
            a = self.lower_scalar(a);
            b = self.lower_scalar(b);
        }
        let same = self.b.ins().icmp(IntCC::Equal, a, b);
        let k2 = self.b.ins().iadd_imm(k, 1);
        self.branch(same, scalar, &[k2.into()], done, &[no.into()]);

        self.b.switch_to_block(done);
        self.b.block_params(done)[0]
    }

    /// Whether some byte of `p[..len]` matches `needle`, and the index of
    /// the first one (0 when none does).
    fn find_first(
        &mut self,
        p: ir::Value,
        len: ir::Value,
        needle: Needle,
        simd: bool,
    ) -> (ir::Value, ir::Value) {
        let zero = self.zero();
        let scalar = self.b.create_block();
        let scalar_body = self.b.create_block();
        let done = self.b.create_block();
        self.b.append_block_param(scalar, types::I64);
        self.b.append_block_param(done, types::I64);
        self.b.append_block_param(done, types::I64);
        let no = self.b.ins().iconst(types::I64, 0);
        let yes = self.b.ins().iconst(types::I64, 1);
        if simd {
            let end = self.vector_end(len);
            let splat = self.needle_splat(needle);
            let head = self.b.create_block();
            let body = self.b.create_block();
            let next = self.b.create_block();
            let hit = self.b.create_block();
            self.b.append_block_param(head, types::I64);
            self.b.append_block_param(body, types::I64);
            self.jump(head, &[zero.into()]);

            self.b.switch_to_block(head);
            let k = self.b.block_params(head)[0];
            let more = self.b.ins().icmp(IntCC::UnsignedLessThan, k, end);
            self.vector_head_exit(more, k, len, body, scalar);

            self.b.switch_to_block(body);
            let k = self.b.block_params(body)[0];
            let at = self.b.ins().iadd(p, k);
            let v = self.b.ins().load(types::I8X16, little(), at, 0);
            let found = self.needle_vector(splat, v);
            let bits = self.b.ins().vhigh_bits(types::I32, found);
            self.branch(bits, hit, &[], next, &[]);

            self.b.switch_to_block(next);
            let k2 = self.b.ins().iadd_imm(k, LANES);
            self.jump(head, &[k2.into()]);

            self.b.switch_to_block(hit);
            let first = self.b.ins().ctz(bits);
            let first = self.b.ins().uextend(types::I64, first);
            let index = self.b.ins().iadd(k, first);
            self.jump(done, &[yes.into(), index.into()]);
        } else {
            self.jump(scalar, &[zero.into()]);
        }

        self.b.switch_to_block(scalar);
        let k = self.b.block_params(scalar)[0];
        let more = self.b.ins().icmp(IntCC::UnsignedLessThan, k, len);
        self.branch(more, scalar_body, &[], done, &[no.into(), zero.into()]);

        self.b.switch_to_block(scalar_body);
        let at = self.b.ins().iadd(p, k);
        let b = self.b.ins().uload8(types::I64, MemFlags::new(), at, 0);
        let found = self.needle_scalar(needle, b);
        let k2 = self.b.ins().iadd_imm(k, 1);
        self.branch(found, done, &[yes.into(), k.into()], scalar, &[k2.into()]);

        self.b.switch_to_block(done);
        let present = self.b.block_params(done)[0];
        let index = self.b.block_params(done)[1];
        (present, index)
    }

    /// Whether byte `b` (a zero-extended `i64`) matches `needle`.
    fn needle_scalar(&mut self, needle: Needle, b: ir::Value) -> ir::Value {
        match needle {
            Needle::One(n) => self.b.ins().icmp(IntCC::Equal, b, n),
            Needle::Either(x, y) => {
                let at_x = self.b.ins().icmp(IntCC::Equal, b, x);
                let at_y = self.b.ins().icmp(IntCC::Equal, b, y);
                self.b.ins().bor(at_x, at_y)
            }
            Needle::NotIn { lo, hi, high } => {
                // Bit `b & 63` of `lo` (b < 64) or `hi` (b < 128), else `high`.
                let low_half = self.b.ins().icmp_imm(IntCC::UnsignedLessThan, b, 64);
                let word = self.b.ins().select(low_half, lo, hi);
                let shift = self.b.ins().band_imm(b, 63);
                let bit = self.b.ins().ushr(word, shift);
                let bit = self.b.ins().band_imm(bit, 1);
                let ascii = self.b.ins().icmp_imm(IntCC::UnsignedLessThan, b, 128);
                let member = self.b.ins().select(ascii, bit, high);
                self.b.ins().icmp_imm(IntCC::Equal, member, 0)
            }
        }
    }

    /// The loop-invariant vectors of `needle`.
    fn needle_splat(&mut self, needle: Needle) -> Splat {
        let splat8 = |this: &mut Self, v: ir::Value| {
            let byte = this.b.ins().ireduce(types::I8, v);
            this.b.ins().splat(types::I8X16, byte)
        };
        match needle {
            Needle::One(n) => Splat::One(splat8(self, n)),
            Needle::Either(x, y) => Splat::Either(splat8(self, x), splat8(self, y)),
            Needle::NotIn { lo, hi, high } => {
                // Little-endian lanes: byte j of the table is byte j of
                // `lo` (j < 8) or byte j - 8 of `hi`.
                let table = self.b.ins().splat(types::I64X2, lo);
                let table = self.b.ins().insertlane(table, hi, 1);
                let table = self.b.ins().bitcast(types::I8X16, little(), table);
                let mut bits = [0u8; 16];
                for (k, bit) in bits.iter_mut().enumerate() {
                    *bit = 1 << (k & 7);
                }
                let bits = self
                    .b
                    .func
                    .dfg
                    .constants
                    .insert(ir::ConstantData::from(&bits[..]));
                let bits = self.b.ins().vconst(types::I8X16, bits);
                let seven = self.b.ins().iconst(types::I8, 7);
                let sevens = self.b.ins().splat(types::I8X16, seven);
                let zero = self.b.ins().iconst(types::I8, 0);
                let zeros = self.b.ins().splat(types::I8X16, zero);
                // 0 or all ones.
                let high = self.b.ins().ineg(high);
                let high = splat8(self, high);
                Splat::NotIn {
                    table,
                    bits,
                    sevens,
                    zeros,
                    high,
                }
            }
        }
    }

    /// The lanes of `v` (an `i8x16`) that match: all ones, else zero.
    fn needle_vector(&mut self, splat: Splat, v: ir::Value) -> ir::Value {
        match splat {
            Splat::One(n) => self.b.ins().icmp(IntCC::Equal, v, n),
            Splat::Either(x, y) => {
                let at_x = self.b.ins().icmp(IntCC::Equal, v, x);
                let at_y = self.b.ins().icmp(IntCC::Equal, v, y);
                self.b.ins().bor(at_x, at_y)
            }
            Splat::NotIn {
                table,
                bits,
                sevens,
                zeros,
                high,
            } => {
                // `swizzle` yields 0 for an index >= 16, so a byte >= 128
                // (row 16..31) reads no class bit.
                let row = self.b.ins().ushr_imm(v, 3);
                let row = self.b.ins().swizzle(table, row);
                let column = self.b.ins().band(v, sevens);
                let bit = self.b.ins().swizzle(bits, column);
                let member = self.b.ins().band(row, bit);
                let outside = self.b.ins().icmp(IntCC::Equal, member, zeros);
                let upper = self.b.ins().icmp(IntCC::SignedLessThan, v, zeros);
                let upper_in = self.b.ins().band(upper, high);
                self.b.ins().band_not(outside, upper_in)
            }
        }
    }

    /// `b` (a zero-extended byte in an `i64`) with ASCII `A..=Z` mapped to
    /// `a..=z`.
    fn lower_scalar(&mut self, b: ir::Value) -> ir::Value {
        let from_a = self.b.ins().iadd_imm(b, -i64::from(b'A'));
        let upper = self.b.ins().icmp_imm(IntCC::UnsignedLessThan, from_a, 26);
        let upper = self.b.ins().uextend(types::I64, upper);
        let case_bit = self.b.ins().ishl_imm(upper, 5);
        self.b.ins().bor(b, case_bit)
    }

    /// The lanes of `v` (an `i8x16`) with ASCII `A..=Z` mapped to `a..=z`.
    fn lower_vector(&mut self, v: ir::Value) -> ir::Value {
        let splat = |this: &mut Self, byte: u8| {
            let c = this.b.ins().iconst(types::I8, i64::from(byte));
            this.b.ins().splat(types::I8X16, c)
        };
        let a = splat(self, b'A');
        let span = splat(self, 26);
        let case_bit = splat(self, 0x20);
        let from_a = self.b.ins().isub(v, a);
        let upper = self.b.ins().icmp(IntCC::UnsignedLessThan, from_a, span);
        let add = self.b.ins().band(upper, case_bit);
        self.b.ins().bor(v, add)
    }

    /// Writes `src[..n]` lowercased to `dst[..n]`; the two do not overlap.
    fn lowercase_copy(&mut self, dst: ir::Value, src: ir::Value, n: ir::Value, simd: bool) {
        let zero = self.zero();
        let scalar = self.b.create_block();
        let scalar_body = self.b.create_block();
        let done = self.b.create_block();
        self.b.append_block_param(scalar, types::I64);
        if simd {
            let end = self.vector_end(n);
            let head = self.b.create_block();
            let body = self.b.create_block();
            self.b.append_block_param(head, types::I64);
            self.b.append_block_param(body, types::I64);
            self.jump(head, &[zero.into()]);

            self.b.switch_to_block(head);
            let k = self.b.block_params(head)[0];
            let more = self.b.ins().icmp(IntCC::UnsignedLessThan, k, end);
            self.vector_head_exit(more, k, n, body, scalar);

            self.b.switch_to_block(body);
            let k = self.b.block_params(body)[0];
            let from = self.b.ins().iadd(src, k);
            let v = self.b.ins().load(types::I8X16, little(), from, 0);
            let v = self.lower_vector(v);
            let to = self.b.ins().iadd(dst, k);
            self.b.ins().store(little(), v, to, 0);
            let k2 = self.b.ins().iadd_imm(k, LANES);
            self.jump(head, &[k2.into()]);
        } else {
            self.jump(scalar, &[zero.into()]);
        }

        self.b.switch_to_block(scalar);
        let k = self.b.block_params(scalar)[0];
        let more = self.b.ins().icmp(IntCC::UnsignedLessThan, k, n);
        self.branch(more, scalar_body, &[], done, &[]);

        self.b.switch_to_block(scalar_body);
        let from = self.b.ins().iadd(src, k);
        let b = self.b.ins().uload8(types::I64, MemFlags::new(), from, 0);
        let b = self.lower_scalar(b);
        let to = self.b.ins().iadd(dst, k);
        self.b.ins().istore8(MemFlags::new(), b, to, 0);
        let k2 = self.b.ins().iadd_imm(k, 1);
        self.jump(scalar, &[k2.into()]);

        self.b.switch_to_block(done);
    }
}
