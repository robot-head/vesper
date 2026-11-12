//! §36.5 DetFloat auditor over real Cranelift 0.125 functions.
//!
//! [`audit`] checks a `cranelift_codegen::ir::Function` for the codegen
//! patterns that break cross-ISA bit determinism and returns one
//! [`VerifierFinding`] per violation:
//!
//! * `detfloat.fma`: a fused multiply-add (`fma` instruction or the `FmaF32`
//!   / `FmaF64` libcall). Two-rounded `fmul` + `fadd` is fine; an explicit
//!   `DetFloat::fma` must be allowed by [`DetFloatPolicy::allow_fma`].
//! * `detfloat.libm`: a call to a known libm transcendental (`sinf`, `acos`,
//!   ...) that is not on [`DetFloatPolicy::allowed_float_calls`].
//! * `detfloat.unlisted-float-call`: any other direct call whose signature
//!   carries floats and whose callee is not allowlisted (its rounding is
//!   unknown).
//! * `detfloat.indirect-float-call`: an indirect call with floats in its
//!   signature (the callee cannot be checked).
//! * `detfloat.reduction-order`: a horizontal reduction (a tree of `fadd` or
//!   `fmul` over every lane `extractlane`d from one vector, each lane exactly
//!   once) whose shape is not the order the policy marks
//!   ([`ReductionOrder`]). Operand order inside a node is not significant.
//! * `detfloat.nan-escape`: a value produced by NaN-generating float
//!   arithmetic (the set Cranelift's own NaN canonicalisation pass
//!   rewrites: `fadd`, `fsub`, `fmul`, `fdiv`, `sqrt`, `fma`, `fmin`, `fmax`,
//!   `ceil`, `floor`, `trunc`, `nearest`, precision conversions) reaches a store, a return, a call
//!   argument or an integer `bitcast` without passing through the
//!   canonicalisation pattern `select(fcmp uno x, x, canonical_nan, x)` (or
//!   its `bitselect` vector form). x86-64 and AArch64 produce different
//!   default NaN bit patterns, so such a value can differ across ISAs. Skipped
//!   when [`DetFloatPolicy::codegen_canonicalizes_nans`] says the function is
//!   compiled with Cranelift's `enable_nan_canonicalization`.
//!
//! Cranelift IR has no fast-math or reassociation flags and fixes rounding
//! to round-to-nearest-even, so there is no rounding-mode rule. The auditor
//! looks at one function's IR; it does not check what an ISA backend later
//! emits, and it is not the compiler's `eff [DetFloat]` source effect.

use cranelift_codegen::ir::condcodes::FloatCC;
use cranelift_codegen::ir::instructions::{CallInfo, InstructionData};
use cranelift_codegen::ir::{
    ExternalName, Function, Inst, InstructionData as Data, LibCall, Opcode, Signature, Type, Value,
    ValueDef,
};
use cranelift_codegen::settings;

/// Shared wave-five verifier finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifierFinding {
    pub function: String,
    /// Index of the instruction in layout order, when there is one.
    pub inst: Option<u32>,
    pub rule: &'static str,
    pub message: String,
}

/// Required shape of horizontal reductions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReductionOrder {
    /// Canonical binary tree: `(l0 ∘ l1) ∘ (l2 ∘ l3)`, recursively halving.
    PairwiseTree,
    /// Left fold: `((l0 ∘ l1) ∘ l2) ∘ l3`.
    Sequential,
}

/// Annotations the source language cannot express yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetFloatPolicy {
    /// Allow explicit fused multiply-add (`DetFloat::fma` on both ISAs).
    pub allow_fma: bool,
    /// Callees with floats in their signature that are correctly rounded or
    /// project-implemented (e.g. CORE-MATH `sinf`).
    pub allowed_float_calls: Vec<String>,
    /// Symbol names for `ExternalName::User { namespace, index }` callees.
    pub user_names: Vec<((u32, u32), String)>,
    /// Required reduction shape.
    pub reduction_order: ReductionOrder,
    /// The function is compiled with `enable_nan_canonicalization`.
    pub codegen_canonicalizes_nans: bool,
}

impl Default for DetFloatPolicy {
    fn default() -> Self {
        Self {
            allow_fma: false,
            allowed_float_calls: Vec::new(),
            user_names: Vec::new(),
            reduction_order: ReductionOrder::PairwiseTree,
            codegen_canonicalizes_nans: false,
        }
    }
}

impl DetFloatPolicy {
    /// Take `codegen_canonicalizes_nans` from the Cranelift flags the
    /// function will be compiled with.
    pub fn with_flags(mut self, flags: &settings::Flags) -> Self {
        self.codegen_canonicalizes_nans = flags.enable_nan_canonicalization();
        self
    }
}

/// The function could not be audited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditError {
    /// Cranelift's verifier rejected the IR.
    InvalidIr(String),
}

impl std::fmt::Display for AuditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidIr(e) => write!(f, "invalid Cranelift IR: {e}"),
        }
    }
}

impl std::error::Error for AuditError {}

/// libm transcendentals whose platform implementations are not guaranteed
/// to be correctly rounded.
pub const LIBM_TRANSCENDENTALS: &[&str] = &[
    "sin",
    "cos",
    "tan",
    "asin",
    "acos",
    "atan",
    "atan2",
    "sinh",
    "cosh",
    "tanh",
    "asinh",
    "acosh",
    "atanh",
    "exp",
    "exp2",
    "exp10",
    "expm1",
    "log",
    "log2",
    "log10",
    "log1p",
    "pow",
    "cbrt",
    "hypot",
    "erf",
    "erfc",
    "tgamma",
    "lgamma",
    "sincos",
    "fmod",
    "remainder",
];

fn is_libm(name: &str) -> bool {
    let base = name.strip_suffix('f').unwrap_or(name);
    LIBM_TRANSCENDENTALS.contains(&base) || LIBM_TRANSCENDENTALS.contains(&name)
}

fn is_float(ty: Type) -> bool {
    ty.lane_type().is_float()
}

fn sig_has_float(sig: &Signature) -> bool {
    sig.params
        .iter()
        .chain(&sig.returns)
        .any(|p| is_float(p.value_type))
}

struct Ctx<'a> {
    func: &'a Function,
    name: String,
    findings: Vec<VerifierFinding>,
    index: Vec<u32>,
}

impl Ctx<'_> {
    fn report(&mut self, inst: Option<Inst>, rule: &'static str, message: String) {
        self.findings.push(VerifierFinding {
            function: self.name.clone(),
            inst: inst.map(|i| self.index[i.as_u32() as usize]),
            rule,
            message,
        });
    }

    fn data(&self, inst: Inst) -> &InstructionData {
        &self.func.dfg.insts[inst]
    }

    fn def_inst(&self, v: Value) -> Option<Inst> {
        match self.func.dfg.value_def(v) {
            ValueDef::Result(inst, _) => Some(inst),
            _ => None,
        }
    }
}

/// Audit `func` against `policy`.
pub fn audit(func: &Function, policy: &DetFloatPolicy) -> Result<Vec<VerifierFinding>, AuditError> {
    let flags = settings::Flags::new(settings::builder());
    cranelift_codegen::verify_function(func, &flags)
        .map_err(|e| AuditError::InvalidIr(e.to_string()))?;
    let mut index = vec![u32::MAX; func.dfg.num_insts()];
    let mut order = Vec::new();
    for block in func.layout.blocks() {
        for inst in func.layout.block_insts(block) {
            index[inst.as_u32() as usize] = order.len() as u32;
            order.push(inst);
        }
    }
    let mut cx = Ctx {
        func,
        name: func.name.to_string(),
        findings: Vec::new(),
        index,
    };
    check_fma_and_calls(&mut cx, &order, policy);
    check_reductions(&mut cx, &order, policy.reduction_order);
    if !policy.codegen_canonicalizes_nans {
        check_nan_escape(&mut cx, &order);
    }
    Ok(cx.findings)
}

fn callee_name(func: &Function, name: &ExternalName, policy: &DetFloatPolicy) -> String {
    match name {
        ExternalName::User(r) => {
            let user = &func.params.user_named_funcs()[*r];
            policy
                .user_names
                .iter()
                .find(|(k, _)| *k == (user.namespace, user.index))
                .map(|(_, n)| n.clone())
                .unwrap_or_else(|| format!("u{}:{}", user.namespace, user.index))
        }
        ExternalName::TestCase(t) => t.to_string().trim_start_matches('%').to_owned(),
        ExternalName::LibCall(l) => l.to_string(),
        ExternalName::KnownSymbol(k) => format!("{k:?}"),
    }
}

fn check_fma_and_calls(cx: &mut Ctx<'_>, order: &[Inst], policy: &DetFloatPolicy) {
    let func = cx.func;
    for &inst in order {
        let data = cx.data(inst);
        if data.opcode() == Opcode::Fma && !policy.allow_fma {
            cx.report(
                Some(inst),
                "detfloat.fma",
                "fused multiply-add rounds once; DetFloat requires two-rounded fmul + fadd \
                 unless explicit DetFloat::fma is allowed"
                    .into(),
            );
            continue;
        }
        match data.analyze_call(&func.dfg.value_lists, &func.dfg.exception_tables) {
            CallInfo::Direct(func_ref, _) | CallInfo::DirectWithSig(func_ref, _, _) => {
                let ext = &func.dfg.ext_funcs[func_ref];
                let sig = &func.dfg.signatures[ext.signature];
                if let ExternalName::LibCall(lib) = &ext.name {
                    match lib {
                        LibCall::FmaF32 | LibCall::FmaF64 if !policy.allow_fma => {
                            cx.report(
                                Some(inst),
                                "detfloat.fma",
                                format!("call to fused multiply-add libcall {lib}"),
                            );
                            continue;
                        }
                        // Exact IEEE-754 operations.
                        LibCall::CeilF32
                        | LibCall::CeilF64
                        | LibCall::FloorF32
                        | LibCall::FloorF64
                        | LibCall::TruncF32
                        | LibCall::TruncF64
                        | LibCall::NearestF32
                        | LibCall::NearestF64
                        | LibCall::FmaF32
                        | LibCall::FmaF64 => continue,
                        _ => {}
                    }
                }
                if !sig_has_float(sig) {
                    continue;
                }
                let name = callee_name(func, &ext.name, policy);
                if policy.allowed_float_calls.contains(&name) {
                    continue;
                }
                if is_libm(&name) {
                    cx.report(
                        Some(inst),
                        "detfloat.libm",
                        format!(
                            "call to platform libm `{name}`, which is not guaranteed correctly \
                             rounded; route it through an allowlisted correctly-rounded \
                             implementation"
                        ),
                    );
                } else {
                    cx.report(
                        Some(inst),
                        "detfloat.unlisted-float-call",
                        format!("float call to `{name}`, which is not on the DetFloat allowlist"),
                    );
                }
            }
            CallInfo::Indirect(sig_ref, _) => {
                if sig_has_float(&func.dfg.signatures[sig_ref]) {
                    cx.report(
                        Some(inst),
                        "detfloat.indirect-float-call",
                        "indirect call with float arguments or results cannot be checked".into(),
                    );
                }
            }
            CallInfo::NotACall => {}
        }
    }
}

// ---------------------------------------------------------------- reductions

#[derive(Debug, Clone, PartialEq, Eq)]
enum Shape {
    Leaf(u8),
    Node(Box<Shape>, Box<Shape>),
}

impl Shape {
    fn min_lane(&self) -> u8 {
        match self {
            Shape::Leaf(l) => *l,
            Shape::Node(a, b) => a.min_lane().min(b.min_lane()),
        }
    }

    /// Children ordered by smallest lane (operand order is not significant).
    fn normalized(self) -> Self {
        match self {
            Shape::Leaf(l) => Shape::Leaf(l),
            Shape::Node(a, b) => {
                let (a, b) = (a.normalized(), b.normalized());
                if a.min_lane() <= b.min_lane() {
                    Shape::Node(Box::new(a), Box::new(b))
                } else {
                    Shape::Node(Box::new(b), Box::new(a))
                }
            }
        }
    }

    fn pairwise(lo: u8, hi: u8) -> Self {
        if hi - lo == 1 {
            Shape::Leaf(lo)
        } else {
            let mid = lo + (hi - lo) / 2;
            Shape::Node(
                Box::new(Self::pairwise(lo, mid)),
                Box::new(Self::pairwise(mid, hi)),
            )
        }
    }

    fn sequential(lanes: u8) -> Self {
        let mut s = Shape::Leaf(0);
        for l in 1..lanes {
            s = Shape::Node(Box::new(s), Box::new(Shape::Leaf(l)));
        }
        s
    }
}

#[derive(Debug, Clone)]
struct Tree {
    vector: Value,
    op: Option<Opcode>,
    mask: u64,
    shape: Shape,
}

fn lane_tree(cx: &Ctx<'_>, v: Value, memo: &mut Vec<Option<Option<Tree>>>) -> Option<Tree> {
    let slot = v.as_u32() as usize;
    if let Some(done) = &memo[slot] {
        return done.clone();
    }
    let result = (|| {
        let inst = cx.def_inst(v)?;
        match *cx.data(inst) {
            Data::BinaryImm8 {
                opcode: Opcode::Extractlane,
                arg,
                imm,
            } => {
                let ty = cx.func.dfg.value_type(arg);
                if !is_float(ty) || ty.lane_count() > 64 {
                    return None;
                }
                Some(Tree {
                    vector: arg,
                    op: None,
                    mask: 1u64 << imm,
                    shape: Shape::Leaf(imm),
                })
            }
            Data::Binary {
                opcode: op @ (Opcode::Fadd | Opcode::Fmul),
                args: [x, y],
            } => {
                let a = lane_tree(cx, x, memo)?;
                let b = lane_tree(cx, y, memo)?;
                let same_op = a.op.is_none_or(|o| o == op) && b.op.is_none_or(|o| o == op);
                if a.vector != b.vector || !same_op || a.mask & b.mask != 0 {
                    return None;
                }
                Some(Tree {
                    vector: a.vector,
                    op: Some(op),
                    mask: a.mask | b.mask,
                    shape: Shape::Node(Box::new(a.shape), Box::new(b.shape)),
                })
            }
            _ => None,
        }
    })();
    memo[slot] = Some(result.clone());
    result
}

fn check_reductions(cx: &mut Ctx<'_>, order: &[Inst], required: ReductionOrder) {
    let mut memo = vec![None; cx.func.dfg.num_values()];
    let mut found = Vec::new();
    for &inst in order {
        let data = cx.data(inst);
        if !matches!(data.opcode(), Opcode::Fadd | Opcode::Fmul) {
            continue;
        }
        let Some(&result) = cx.func.dfg.inst_results(inst).first() else {
            continue;
        };
        let Some(tree) = lane_tree(cx, result, &mut memo) else {
            continue;
        };
        let lanes = cx.func.dfg.value_type(tree.vector).lane_count();
        let full = if lanes == 64 {
            u64::MAX
        } else {
            (1u64 << lanes) - 1
        };
        if lanes < 2 || tree.mask != full {
            continue;
        }
        let lanes = lanes as u8;
        let expected = match required {
            ReductionOrder::PairwiseTree => Shape::pairwise(0, lanes),
            ReductionOrder::Sequential => Shape::sequential(lanes),
        };
        if tree.shape.clone().normalized() != expected.normalized() {
            found.push((inst, tree.op, lanes));
        }
    }
    for (inst, op, lanes) in found {
        cx.report(
            Some(inst),
            "detfloat.reduction-order",
            format!(
                "{}-lane horizontal {} reduction is not in the policy's {:?} order",
                lanes,
                op.map_or("float", |o| if o == Opcode::Fadd { "fadd" } else { "fmul" }),
                required
            ),
        );
    }
}

// ----------------------------------------------------------------- NaN escape

fn generates_nan(op: Opcode) -> bool {
    matches!(
        op,
        Opcode::Fadd
            | Opcode::Fsub
            | Opcode::Fmul
            | Opcode::Fdiv
            | Opcode::Sqrt
            | Opcode::Fma
            | Opcode::Fmin
            | Opcode::Fmax
            | Opcode::Ceil
            | Opcode::Floor
            | Opcode::Trunc
            | Opcode::Nearest
            | Opcode::Fpromote
            | Opcode::Fdemote
            | Opcode::Fvdemote
            | Opcode::FvpromoteLow
    )
}

fn canonical_nan_bits(ty: Type, bits: &[u8]) -> bool {
    match ty.lane_type().bits() {
        32 => bits
            .chunks_exact(4)
            .all(|c| u32::from_le_bytes(c.try_into().expect("4")) == 0x7fc0_0000),
        64 => bits
            .chunks_exact(8)
            .all(|c| u64::from_le_bytes(c.try_into().expect("8")) == 0x7ff8_0000_0000_0000),
        _ => false,
    }
}

impl Ctx<'_> {
    /// `v` is a constant (scalar, splat or vconst) whose every lane is the
    /// canonical quiet NaN.
    fn is_canonical_nan_const(&self, v: Value) -> bool {
        let Some(inst) = self.def_inst(v) else {
            return false;
        };
        let ty = self.func.dfg.value_type(v);
        match *self.data(inst) {
            Data::UnaryIeee32 { imm, .. } => imm.bits() == 0x7fc0_0000,
            Data::UnaryIeee64 { imm, .. } => imm.bits() == 0x7ff8_0000_0000_0000,
            Data::Unary {
                opcode: Opcode::Splat,
                arg,
            } => self.is_canonical_nan_const(arg),
            // Cranelift's scalar-via-vector canonicalisation: lane 0 holds
            // the constant; the other lanes are only selected where the
            // (equally `scalar_to_vector`ed, hence zero) input lane is NaN,
            // which never happens.
            Data::Unary {
                opcode: Opcode::ScalarToVector,
                arg,
            } => self.is_canonical_nan_const(arg),
            Data::UnaryConst {
                opcode: Opcode::Vconst,
                constant_handle,
            } => canonical_nan_bits(ty, self.func.dfg.constants.get(constant_handle).as_slice()),
            _ => false,
        }
    }

    /// `c` is `fcmp uno x, x` / `fcmp ne x, x` (`nan == true`) or
    /// `fcmp ord x, x` / `fcmp eq x, x` (`nan == false`).
    fn is_nan_test_of(&self, c: Value, x: Value, nan: bool) -> bool {
        let Some(mut inst) = self.def_inst(c) else {
            return false;
        };
        // Cranelift's own vector canonicalisation bitcasts the lane mask to
        // the float vector type before `bitselect`.
        if let Data::LoadNoOffset {
            opcode: Opcode::Bitcast,
            arg,
            ..
        } = *self.data(inst)
        {
            let Some(inner) = self.def_inst(arg) else {
                return false;
            };
            inst = inner;
        }
        let Data::FloatCompare {
            cond, args: [a, b], ..
        } = *self.data(inst)
        else {
            return false;
        };
        let is_nan = match cond {
            FloatCC::Unordered | FloatCC::NotEqual => true,
            FloatCC::Ordered | FloatCC::Equal => false,
            _ => return false,
        };
        a == x && b == x && is_nan == nan
    }

    /// `inst` is `select/bitselect(fcmp uno x x, canonical_nan, x)`.
    fn is_canonicalizer(&self, inst: Inst) -> bool {
        let op = self.data(inst).opcode();
        if !matches!(
            op,
            Opcode::Select | Opcode::SelectSpectreGuard | Opcode::Bitselect
        ) {
            return false;
        }
        let args = self.func.dfg.inst_args(inst);
        let [c, t, f] = args else { return false };
        (self.is_nan_test_of(*c, *f, true) && self.is_canonical_nan_const(*t))
            || (self.is_nan_test_of(*c, *t, false) && self.is_canonical_nan_const(*f))
    }
}

fn check_nan_escape(cx: &mut Ctx<'_>, order: &[Inst]) {
    let func = cx.func;
    let dfg = &func.dfg;
    let mut tainted = vec![false; dfg.num_values()];
    // Monotone fixpoint over instruction results and block parameters.
    loop {
        let mut changed = false;
        for &inst in order {
            let data = cx.data(inst);
            let op = data.opcode();
            let results = dfg.inst_results(inst);
            let args = dfg.inst_args(inst);
            let any_arg = args.iter().any(|a| tainted[a.as_u32() as usize]);
            let taint_results = if generates_nan(op) {
                true
            } else if cx.is_canonicalizer(inst) {
                false
            } else {
                any_arg && !data.opcode().is_call()
            };
            if taint_results {
                for &r in results {
                    let t = &mut tainted[r.as_u32() as usize];
                    if is_float(dfg.value_type(r)) && !*t {
                        *t = true;
                        changed = true;
                    }
                }
            }
            {
                for call in data.branch_destination(&dfg.jump_tables, &dfg.exception_tables) {
                    let params = func.dfg.block_params(call.block(&dfg.value_lists));
                    for (arg, &param) in call.args(&dfg.value_lists).zip(params) {
                        if let Some(arg) = arg.as_value()
                            && tainted[arg.as_u32() as usize]
                            && !tainted[param.as_u32() as usize]
                        {
                            tainted[param.as_u32() as usize] = true;
                            changed = true;
                        }
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut escapes = Vec::new();
    for &inst in order {
        let data = cx.data(inst);
        let op = data.opcode();
        let args = dfg.inst_args(inst);
        let sink: Option<(&'static str, Vec<Value>)> = match op {
            Opcode::Store | Opcode::StackStore | Opcode::AtomicStore => {
                Some(("stored to memory", vec![args[0]]))
            }
            Opcode::Return => Some(("returned", args.to_vec())),
            _ if op.is_call() => Some(("passed to a call", args.to_vec())),
            Opcode::Bitcast => {
                let r = dfg.inst_results(inst)[0];
                (!is_float(dfg.value_type(r))).then(|| ("bitcast to an integer", vec![args[0]]))
            }
            _ => None,
        };
        if let Some((how, values)) = sink
            && let Some(v) = values.iter().find(|v| tainted[v.as_u32() as usize])
        {
            escapes.push((inst, how, *v));
        }
    }
    for (inst, how, v) in escapes {
        cx.report(
            Some(inst),
            "detfloat.nan-escape",
            format!(
                "float value {v} from NaN-producing arithmetic is {how} without NaN \
                 canonicalisation; x86-64 and AArch64 produce different default NaN bits"
            ),
        );
    }
}
