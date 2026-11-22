//! Native lowering of fixed-size array writes and constant tables (§5,
//! P3b gaps lane 6, #1091).
//!
//! An array `[T; N]` is the homogeneous tuple of `N` elements, so its
//! layout, copies and reads (`a[i]`, `Op::Index`) are the tuple's. This file
//! adds the two forms the tuple lowering lacks:
//!
//! - [`Codegen::array_write`]: `a[i] op= v` and `a.set(i, v)`, which
//!   `crate::arrays` lowers to `Op::Write { op: "[]op", value: (i, v) }`
//!   on the array's place. The element is stored in place at
//!   `&a + i * stride` after one unsigned bounds check, which raises
//!   `Fail[RuntimeError]` (`IndexOutOfBounds`) where the checker recorded a
//!   possible failure and traps where it proved the index in bounds.
//! - [`Codegen::constant_table`]: a call of a module constant whose value is
//!   a constant aggregate reads it from read-only data instead of building
//!   it. Every consumer copies an aggregate before writing it (a `let mut`
//!   binding owns a fresh slot, a field store copies the words), so the
//!   data is never written.
use super::*;

impl<M: Module> Codegen<'_, '_, M> {
    /// `l[i] op= v` for the `(i, v)` pair `r` (see the module docs).
    pub(super) fn array_write(
        &mut self,
        e: &Expr,
        op: &str,
        l: &Expr,
        r: &Expr,
    ) -> Result<ir::Value, String> {
        let pair_ty = self.ty(r);
        let Type::Tuple(pair) = &pair_ty else {
            return Err("native array write requires an (index, value) pair".into());
        };
        let pair_layout = Layout::of(self.world, &pair_ty)?;
        let LayoutKind::Product(pair_fields) = &pair_layout.kind else {
            return Err("native array write requires an (index, value) pair".into());
        };
        let pair_ptr = self.address(r)?;
        let index = self.load(pair_ptr, &pair[0])?;
        let value_at = self.offset(pair_ptr, pair_fields[1].offset);
        let value = self.load(value_at, &pair[1])?;
        self.array_store(&e.span, op, l, index, value, &pair[1])
    }

    /// `l[index] op= value` at `span` with the operands as values: the
    /// write of an `Op::Write` whose pair is built only for it
    /// ([`written_pair`]). `element_ty` is the checked element type, which
    /// selects the arithmetic of a compound write (the local's own type may
    /// still hold the unrefined literal types of its initializer).
    pub(super) fn array_store(
        &mut self,
        span: &Span,
        op: &str,
        l: &Expr,
        index: ir::Value,
        value: ir::Value,
        element_ty: &Type,
    ) -> Result<ir::Value, String> {
        let ty = self.ty(l);
        let layout = Layout::of(self.world, &ty)?;
        let LayoutKind::Product(fs) = &layout.kind else {
            return Err("native array write requires an array".into());
        };
        let mut element = fs.first().ok_or("write into an empty array")?.clone();
        element.ty = crate::numeric::default_type(element_ty);
        let base = self.address(l)?;
        self.index_check(span, index, fs.len() as u64)?;
        let offset = self.b.ins().imul_imm(index, element.layout.size as i64);
        let address = self.b.ins().iadd(base, offset);
        let assign = &op[crate::arrays::WRITE_PREFIX.len()..];
        let value = if assign == "=" {
            value
        } else {
            let old = self.load(address, &element.ty)?;
            let unsigned =
                crate::numeric::IntegerKind::from_type(&element.ty).is_some_and(|k| k.unsigned());
            let checked = crate::numeric::IntegerKind::from_type(&element.ty)
                .filter(|_| self.world.overflow_ops.contains(&(span.start, span.end)));
            let v = if let Some(kind) = checked {
                self.checked_arithmetic(&assign[..1], kind, old, value)?
            } else {
                let flush = self.flushes(span);
                let old = self.flush(flush, old);
                let value = self.flush(flush, value);
                let r = self.binary(&assign[..1], old, value, unsigned)?;
                self.flush(flush, r)
            };
            self.coerce(v, Some(&element.ty))
        };
        self.store(address, value, &element.ty)?;
        Ok(self.zero())
    }

    /// The bounds check of an index into a `len`-element array or tuple at
    /// `span`: raises `IndexOutOfBounds` where the checker recorded a
    /// possible failure and traps elsewhere. It emits nothing when the
    /// index's own instructions bound it below `len` ([`Self::index_below`]):
    /// a constant, a mask, a remainder. Cranelift does not fold those
    /// compares itself.
    pub(super) fn index_check(
        &mut self,
        span: &Span,
        index: ir::Value,
        len: u64,
    ) -> Result<(), String> {
        if self.index_below(index, len, 4) {
            return Ok(());
        }
        let valid = self
            .b
            .ins()
            .icmp_imm(IntCC::UnsignedLessThan, index, len as i64);
        let good = self.b.create_block();
        let bad = self.b.create_block();
        self.branch(valid, good, &[], bad, &[]);
        self.b.switch_to_block(bad);
        if self.world.partial_ops.contains(&(span.start, span.end)) {
            self.primitive_failure("IndexOutOfBounds")?;
            self.jump(good, &[]);
        } else {
            self.b.ins().trap(ir::TrapCode::unwrap_user(2));
        }
        self.b.switch_to_block(good);
        Ok(())
    }

    /// Whether the instructions that define `v` show `v < len` as an
    /// unsigned value whatever their operands are: a constant below `len`,
    /// `x & m` with a constant `m < len`, `x % m` with a constant
    /// `0 < m <= len`, a zero extension or a `select` of such values, looked
    /// through at most `depth` levels.
    pub(super) fn index_below(&self, v: ir::Value, len: u64, depth: u32) -> bool {
        use ir::{InstructionData as D, Opcode as O};
        let dfg = &self.b.func.dfg;
        let ir::ValueDef::Result(inst, 0) = dfg.value_def(v) else {
            return false;
        };
        let constant = |x: ir::Value| match dfg.value_def(x) {
            ir::ValueDef::Result(i, 0) => match dfg.insts[i] {
                D::UnaryImm {
                    opcode: O::Iconst,
                    imm,
                } => {
                    // The constant at its own width, as unsigned.
                    let bits = dfg.value_type(x).bits();
                    let raw = imm.bits() as u64;
                    Some(if bits >= 64 {
                        raw
                    } else {
                        raw & ((1u64 << bits) - 1)
                    })
                }
                _ => None,
            },
            _ => None,
        };
        match dfg.insts[inst] {
            D::UnaryImm {
                opcode: O::Iconst, ..
            } => constant(v).is_some_and(|k| k < len),
            D::Binary {
                opcode: O::Band,
                args: [a, b],
            } => [a, b].iter().any(|&x| constant(x).is_some_and(|m| m < len)),
            D::BinaryImm64 {
                opcode: O::BandImm,
                imm,
                ..
            } => (imm.bits() as u64) < len,
            D::Binary {
                opcode: O::Urem,
                args: [_, b],
            } => constant(b).is_some_and(|m| m > 0 && m <= len),
            D::BinaryImm64 {
                opcode: O::UremImm,
                imm,
                ..
            } => {
                let m = imm.bits() as u64;
                m > 0 && m <= len
            }
            D::Unary {
                opcode: O::Uextend,
                arg,
            } => depth > 0 && self.index_below(arg, len, depth - 1),
            D::Ternary {
                opcode: O::Select,
                args: [_, a, b],
            } => {
                depth > 0
                    && self.index_below(a, len, depth - 1)
                    && self.index_below(b, len, depth - 1)
            }
            _ => false,
        }
    }

    /// A constant table read `$table$NAME` (`crate::arrays::lower_world`)
    /// for value `dest` of `graph`: a pointer to the constant's read-only
    /// data when every use of `dest` only reads an element or a field, else
    /// a fresh stack copy of it. `None` for any other callee.
    pub(super) fn constant_table(
        &mut self,
        callee: &str,
        graph: &crate::ir::FunctionIr,
        dest: crate::ir::ValueId,
    ) -> Result<Option<ir::Value>, String> {
        let Some(name) = crate::arrays::table_function(callee) else {
            return Ok(None);
        };
        let f = self
            .world
            .functions
            .get(&name)
            .ok_or("unknown native constant table")?;
        let bytes =
            crate::arrays::constant_bytes(self.world, f)?.ok_or("non-constant native table")?;
        let size = bytes.len() as u32;
        let id = self
            .m
            .declare_anonymous_data(false, false)
            .map_err(|e| e.to_string())?;
        let mut data = cranelift_module::DataDescription::new();
        data.define(bytes.into_boxed_slice());
        data.set_align(8);
        self.m.define_data(id, &data).map_err(|e| e.to_string())?;
        let global = self.m.declare_data_in_func(id, self.b.func);
        let pointer = self.m.target_config().pointer_type();
        let table = self.b.ins().symbol_value(pointer, global);
        if only_read(graph, dest) {
            return Ok(Some(table));
        }
        let copy = self.alloc_uninit(size);
        self.copy_bytes(table, copy, size);
        Ok(Some(copy))
    }
}

/// Whether every use of `id` in `graph` reads an element or a field of it.
fn only_read(graph: &crate::ir::FunctionIr, id: crate::ir::ValueId) -> bool {
    use crate::ir::Op;
    graph.blocks.iter().all(|b| {
        !crate::ir_inline::terminator_operands(&b.terminator).contains(&id)
            && b.instructions.iter().all(|i| match &i.op {
                Op::Index(base, index) => *index != id || *base == id,
                Op::Project(..) => true,
                op => !crate::ir_inline::operands(op).contains(&id),
            })
    })
}

/// Whether instruction `at` of block `block` (a `Read`) has exactly one
/// use, as the base of an `Index` later in the same block, with only pure
/// instructions in between, so the element can be read from the place
/// itself: nothing can write the place before the read.
pub(super) fn read_in_place(graph: &crate::ir::FunctionIr, block: usize, at: usize) -> bool {
    use crate::ir::Op;
    let b = &graph.blocks[block];
    let id = b.instructions[at].dest;
    let Some(k) = b.instructions[at + 1..]
        .iter()
        .position(|i| crate::ir_inline::operands(&i.op).contains(&id))
        .map(|k| k + at + 1)
    else {
        return false;
    };
    if !matches!(b.instructions[k].op, Op::Index(base, index) if base == id && index != id) {
        return false;
    }
    let pure = b.instructions[at + 1..k].iter().all(|i| {
        matches!(
            i.op,
            Op::Constant(_)
                | Op::Unit
                | Op::Read { .. }
                | Op::Unary(..)
                | Op::Binary(..)
                | Op::Index(..)
                | Op::Project(..)
                | Op::Tuple(_)
                | Op::Data(..)
        )
    });
    let later = b.instructions[k + 1..]
        .iter()
        .any(|i| crate::ir_inline::operands(&i.op).contains(&id))
        || crate::ir_inline::terminator_operands(&b.terminator).contains(&id)
        || graph.blocks.iter().enumerate().any(|(n, other)| {
            n != block
                && (other
                    .instructions
                    .iter()
                    .any(|i| crate::ir_inline::operands(&i.op).contains(&id))
                    || crate::ir_inline::terminator_operands(&other.terminator).contains(&id))
        });
    pure && !later
}

/// The `(index, value)` operands of the pair `value` when it is an
/// `Op::Tuple` whose only use is one array element write (`Op::Write` with
/// a [`crate::arrays::WRITE_PREFIX`] operator), so the write can take them
/// directly and the pair need not be built.
pub(super) fn written_pair(
    graph: &crate::ir::FunctionIr,
    value: crate::ir::ValueId,
) -> Option<(crate::ir::ValueId, crate::ir::ValueId)> {
    use crate::ir::Op;
    let mut pair = None;
    let mut uses = 0;
    for b in &graph.blocks {
        if crate::ir_inline::terminator_operands(&b.terminator).contains(&value) {
            return None;
        }
        for i in &b.instructions {
            if i.dest == value {
                let Op::Tuple(ids) = &i.op else {
                    return None;
                };
                let [a, v] = ids.as_slice() else {
                    return None;
                };
                pair = Some((*a, *v));
            }
            let n = crate::ir_inline::operands(&i.op)
                .iter()
                .filter(|o| **o == value)
                .count();
            if n > 0 {
                if !matches!(&i.op, Op::Write { op, .. } if op.starts_with(crate::arrays::WRITE_PREFIX))
                {
                    return None;
                }
                uses += n;
            }
        }
    }
    pair.filter(|_| uses == 1)
}
