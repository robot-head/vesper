//! Per-assumption boundary checks.
//!
//! A `.vsys` signature states assumptions that C cannot express: a borrowed
//! pointer is non-null, aligned, names exactly one element (or `length`
//! elements), does not alias a written borrow, and is not retained. Some of
//! these can be checked numerically at the call boundary, before the call or
//! on its return; others cannot be checked at all. This module reports every
//! assumption separately, so a caller sees which held, which were violated
//! and which remain declared-only.
//!
//! Checks are numeric only: an address range is never dereferenced here.
//! Guard bytes ([`GuardObservation`]) are observed by the (unsafe) caller,
//! which owns the memory; this module only compares what it is given.
use super::foreign::validate;
use super::*;

/// Where an assumption applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Position {
    /// The call as a whole (argument count).
    Call,
    /// One declared parameter, by index.
    Argument(usize),
    /// The foreign return value.
    Return,
}

/// One assumption a `.vsys` signature makes at the foreign boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Assumption {
    /// The argument list matches the declared parameters in count and kind.
    ArgumentShape,
    /// A scalar, bool or enum carrier holds a valid bit pattern.
    ValidBits,
    /// A borrowed pointer, or a returned `Ptr[T]`, is not null.
    NonNull,
    /// The address is aligned for its element/pointee.
    Aligned,
    /// The borrowed byte range is representable: it does not wrap the address
    /// space and is at most `i64::MAX` bytes.
    InBounds,
    /// The range does not overlap another borrowed range when either is
    /// written.
    Disjoint,
    /// The callee touched only the one element a single pointer names.
    /// Checkable after return with a guard observation.
    SingleElement,
    /// The callee stayed within the declared slice length. Checkable after
    /// return with a guard observation.
    WithinLength,
    /// The returned pointer is not derived from this `nocapture` borrow:
    /// it does not lie inside the borrowed range.
    NoCaptureViaReturn,
    /// The callee keeps no copy of this pointer after it returns. Not
    /// observable at a call boundary.
    NoRetention,
}

/// When an assumption can be checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checkability {
    /// By [`FunctionDescriptor::check_arguments`] / `prepare_call`.
    BeforeCall,
    /// By [`FunctionDescriptor::check_boundary`] from the returned bits.
    AfterReturn,
    /// After return, only when the caller supplies a [`GuardObservation`].
    WithGuard,
    /// Never at the boundary; it stays a declared assumption.
    Unverifiable,
}

/// An assumption a function's signature declares, and when it is checkable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredAssumption {
    pub position: Position,
    /// The parameter name for [`Position::Argument`].
    pub parameter: Option<String>,
    pub assumption: Assumption,
    pub checkability: Checkability,
}

/// The result of checking one assumption at one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Held,
    Violated(ValidationError),
    /// The assumption does not apply to this call (for example a `None`
    /// pointer, an empty slice, or a non-pointer result).
    NotApplicable(&'static str),
    /// Applicable but not checked (no guard supplied, or never observable).
    Unchecked(&'static str),
}

/// One checked assumption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssumptionCheck {
    pub position: Position,
    pub parameter: Option<String>,
    pub assumption: Assumption,
    pub outcome: Outcome,
}

/// Every assumption checked at one call, in parameter order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BoundaryReport {
    checks: Vec<AssumptionCheck>,
}
impl BoundaryReport {
    pub fn checks(&self) -> &[AssumptionCheck] {
        &self.checks
    }
    /// Whether no assumption was violated. Unchecked assumptions do not count
    /// as violations; see [`Self::unchecked`].
    pub fn holds(&self) -> bool {
        self.violations().next().is_none()
    }
    pub fn violations(&self) -> impl Iterator<Item = &AssumptionCheck> {
        self.checks
            .iter()
            .filter(|c| matches!(c.outcome, Outcome::Violated(_)))
    }
    pub fn unchecked(&self) -> impl Iterator<Item = &AssumptionCheck> {
        self.checks
            .iter()
            .filter(|c| matches!(c.outcome, Outcome::Unchecked(_)))
    }
    /// The outcome of one assumption at one position, if it was reported.
    pub fn outcome(&self, position: Position, assumption: Assumption) -> Option<&Outcome> {
        self.checks
            .iter()
            .find(|c| c.position == position && c.assumption == assumption)
            .map(|c| &c.outcome)
    }
    pub(super) fn into_first_violation(self) -> Option<(Position, ValidationError)> {
        self.checks.into_iter().find_map(|c| match c.outcome {
            Outcome::Violated(e) => Some((c.position, e)),
            _ => None,
        })
    }
    fn push(
        &mut self,
        position: Position,
        parameter: Option<&str>,
        assumption: Assumption,
        outcome: Outcome,
    ) {
        self.checks.push(AssumptionCheck {
            position,
            parameter: parameter.map(str::to_owned),
            assumption,
            outcome,
        });
    }
}

/// Bytes just past a borrowed range, read by the caller immediately before
/// and immediately after the call. For a single pointer the guard starts at
/// `address + size`; for a slice at `address + length * element_size`.
///
/// The caller must have placed the borrow inside a larger allocation it owns,
/// so that the guard bytes are its own memory and not another object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardObservation {
    pub argument: usize,
    pub before: Vec<u8>,
    pub after: Vec<u8>,
}

/// Largest accepted guard observation.
pub const MAX_GUARD_BYTES: usize = 4096;

/// A borrowed numeric range: argument, first byte, last byte, written.
#[derive(Debug, Clone, Copy)]
struct Borrow {
    argument: usize,
    first: u64,
    last: u64,
    writes: bool,
}

impl FunctionDescriptor {
    /// Every assumption this signature makes, and when each can be checked.
    pub fn assumptions(&self) -> Vec<DeclaredAssumption> {
        use Assumption::*;
        use Checkability::*;
        let mut out = vec![DeclaredAssumption {
            position: Position::Call,
            parameter: None,
            assumption: ArgumentShape,
            checkability: BeforeCall,
        }];
        let returns_pointer = matches!(self.result, ValueType::Address { .. });
        for (i, p) in self.parameters.iter().enumerate() {
            let list: &[(Assumption, Checkability)] = match &p.ty {
                ParameterType::Value(_) => &[(ValidBits, BeforeCall)],
                ParameterType::Slice { .. } => &[
                    (NonNull, BeforeCall),
                    (Aligned, BeforeCall),
                    (InBounds, BeforeCall),
                    (Disjoint, BeforeCall),
                    (WithinLength, WithGuard),
                    (NoCaptureViaReturn, AfterReturn),
                    (NoRetention, Unverifiable),
                ],
                ParameterType::Pointer { .. } => &[
                    (NonNull, BeforeCall),
                    (Aligned, BeforeCall),
                    (InBounds, BeforeCall),
                    (Disjoint, BeforeCall),
                    (SingleElement, WithGuard),
                    (NoCaptureViaReturn, AfterReturn),
                    (NoRetention, Unverifiable),
                ],
            };
            for (assumption, checkability) in list {
                if *assumption == NoCaptureViaReturn && !returns_pointer {
                    continue;
                }
                out.push(DeclaredAssumption {
                    position: Position::Argument(i),
                    parameter: Some(p.name.clone()),
                    assumption: *assumption,
                    checkability: *checkability,
                });
            }
        }
        match &self.result {
            ValueType::Unit | ValueType::Scalar(_) | ValueType::Bool | ValueType::Enum(_) => out
                .push(DeclaredAssumption {
                    position: Position::Return,
                    parameter: None,
                    assumption: ValidBits,
                    checkability: AfterReturn,
                }),
            ValueType::Address { nullable, .. } => {
                if !nullable {
                    out.push(DeclaredAssumption {
                        position: Position::Return,
                        parameter: None,
                        assumption: NonNull,
                        checkability: AfterReturn,
                    });
                }
                out.push(DeclaredAssumption {
                    position: Position::Return,
                    parameter: None,
                    assumption: Aligned,
                    checkability: AfterReturn,
                });
            }
        }
        out
    }

    /// Check every before-call assumption of `args`, without stopping at the
    /// first violation. `prepare_call` fails with the first violation here.
    pub fn check_arguments(&self, args: &[RawArgument]) -> BoundaryReport {
        self.before_call(args).0
    }

    /// Check before-call assumptions, then the after-return ones from the
    /// raw returned bits and the caller's guard observations.
    pub fn check_boundary(
        &self,
        args: &[RawArgument],
        return_bits: u64,
        guards: &[GuardObservation],
    ) -> BoundaryReport {
        let (mut report, borrows) = self.before_call(args);
        if report
            .outcome(Position::Call, Assumption::ArgumentShape)
            .is_some_and(|o| *o != Outcome::Held)
        {
            return report;
        }
        // Guards, per borrowed argument.
        for g in guards {
            let known = self.parameters.get(g.argument).is_some_and(|p| {
                matches!(
                    p.ty,
                    ParameterType::Slice { .. } | ParameterType::Pointer { .. }
                )
            });
            if !known {
                report.push(
                    Position::Call,
                    None,
                    Assumption::ArgumentShape,
                    Outcome::Unchecked("a guard observation names no borrowed argument; ignored"),
                );
            }
        }
        for (i, p) in self.parameters.iter().enumerate() {
            let assumption = match p.ty {
                ParameterType::Slice { .. } => Assumption::WithinLength,
                ParameterType::Pointer { .. } => Assumption::SingleElement,
                ParameterType::Value(_) => continue,
            };
            let borrowed = borrows.iter().any(|b| b.argument == i);
            let mut mine = guards.iter().filter(|g| g.argument == i);
            let outcome = match (borrowed, mine.next(), mine.next()) {
                (false, _, _) => Outcome::NotApplicable("no memory was borrowed"),
                (true, None, _) => Outcome::Unchecked("no guard observation supplied"),
                (true, Some(_), Some(_)) => {
                    Outcome::Unchecked("more than one guard observation for this argument")
                }
                (true, Some(g), None) => guard(g),
            };
            report.push(Position::Argument(i), Some(&p.name), assumption, outcome);
        }
        // The return value, then returned-pointer containment.
        let returned = match &self.result {
            ValueType::Address { .. } => match validate(&self.result, return_bits) {
                Ok(_) => {
                    if !matches!(self.result, ValueType::Address { nullable: true, .. }) {
                        report.push(Position::Return, None, Assumption::NonNull, Outcome::Held);
                    }
                    report.push(Position::Return, None, Assumption::Aligned, Outcome::Held);
                    (return_bits != 0).then_some(return_bits)
                }
                Err(e) => {
                    let a = if e == ValidationError::NullAddress {
                        Assumption::NonNull
                    } else {
                        Assumption::Aligned
                    };
                    report.push(Position::Return, None, a, Outcome::Violated(e));
                    None
                }
            },
            other => {
                let outcome = match validate(other, return_bits) {
                    Ok(_) => Outcome::Held,
                    Err(e) => Outcome::Violated(e),
                };
                report.push(Position::Return, None, Assumption::ValidBits, outcome);
                None
            }
        };
        let returns_pointer = matches!(self.result, ValueType::Address { .. });
        for (i, p) in self.parameters.iter().enumerate() {
            if matches!(p.ty, ParameterType::Value(_)) {
                continue;
            }
            if returns_pointer {
                let borrow = borrows.iter().find(|b| b.argument == i);
                let outcome = match (borrow, returned) {
                    (None, _) => Outcome::NotApplicable("no memory was borrowed"),
                    (Some(_), None) => Outcome::NotApplicable("no non-null pointer was returned"),
                    (Some(b), Some(r)) if b.first <= r && r <= b.last => {
                        Outcome::Violated(ValidationError::ReturnAliasesBorrow { argument: i })
                    }
                    (Some(_), Some(_)) => Outcome::Held,
                };
                report.push(
                    Position::Argument(i),
                    Some(&p.name),
                    Assumption::NoCaptureViaReturn,
                    outcome,
                );
            }
            report.push(
                Position::Argument(i),
                Some(&p.name),
                Assumption::NoRetention,
                Outcome::Unchecked("retention after return is not observable at a call boundary"),
            );
        }
        report
    }

    fn before_call(&self, args: &[RawArgument]) -> (BoundaryReport, Vec<Borrow>) {
        use Assumption::*;
        let mut report = BoundaryReport::default();
        let mut borrows: Vec<Borrow> = vec![];
        if args.len() != self.parameters.len() {
            report.push(
                Position::Call,
                None,
                ArgumentShape,
                Outcome::Violated(ValidationError::ArgumentCount),
            );
            return (report, borrows);
        }
        report.push(Position::Call, None, ArgumentShape, Outcome::Held);
        let na = |why| Outcome::NotApplicable(why);
        for (i, (p, a)) in self.parameters.iter().zip(args).enumerate() {
            let at = Position::Argument(i);
            let name = Some(p.name.as_str());
            // (NonNull, Aligned, InBounds, Disjoint) or a kind mismatch.
            let four = |r: &mut BoundaryReport, outcomes: [Outcome; 4]| {
                for (assumption, outcome) in [NonNull, Aligned, InBounds, Disjoint]
                    .into_iter()
                    .zip(outcomes)
                {
                    r.push(at, name, assumption, outcome);
                }
            };
            match (&p.ty, a) {
                (ParameterType::Value(t), RawArgument::Bits(bits)) => {
                    report.push(
                        at,
                        name,
                        ValidBits,
                        match validate(t, *bits) {
                            Ok(_) => Outcome::Held,
                            Err(e) => Outcome::Violated(e),
                        },
                    );
                }
                (
                    ParameterType::Slice {
                        element,
                        access,
                        nullable,
                    },
                    RawArgument::Slice { address, length },
                ) => {
                    if *address == 0 {
                        let nonnull = match (nullable, *length) {
                            (true, 0) => na("None: a null pointer with length 0"),
                            (true, _) => Outcome::Violated(ValidationError::NullSliceWithLength),
                            (false, _) => Outcome::Violated(ValidationError::NullAddress),
                        };
                        four(&mut report, [nonnull, na("null"), na("null"), na("null")]);
                        continue;
                    }
                    let nonnull = if *nullable {
                        na("Option[CSlice] admits null")
                    } else {
                        Outcome::Held
                    };
                    // Slice element alignments are at most 8.
                    let alignment = element.alignment() as u64;
                    if !address.is_multiple_of(alignment) {
                        let e = ValidationError::MisalignedAddress {
                            alignment: alignment as u8,
                        };
                        four(
                            &mut report,
                            [
                                nonnull,
                                Outcome::Violated(e),
                                na("misaligned"),
                                na("misaligned"),
                            ],
                        );
                        continue;
                    }
                    let range = length
                        .checked_mul(element.size() as u64)
                        .ok_or(ValidationError::SliceRangeOverflow)
                        .and_then(|bytes| {
                            if bytes > i64::MAX as u64 {
                                Err(ValidationError::SliceTooLarge)
                            } else if bytes == 0 {
                                Ok(None)
                            } else {
                                address
                                    .checked_add(bytes - 1)
                                    .map(|last| Some((*address, last)))
                                    .ok_or(ValidationError::SliceRangeOverflow)
                            }
                        });
                    match range {
                        Err(e) => four(
                            &mut report,
                            [
                                nonnull,
                                Outcome::Held,
                                Outcome::Violated(e),
                                na("out of bounds"),
                            ],
                        ),
                        Ok(None) => four(
                            &mut report,
                            [nonnull, Outcome::Held, Outcome::Held, na("empty slice")],
                        ),
                        Ok(Some((first, last))) => {
                            let b = Borrow {
                                argument: i,
                                first,
                                last,
                                writes: *access == Access::Inout,
                            };
                            let disjoint = disjoint(&borrows, &b);
                            borrows.push(b);
                            four(
                                &mut report,
                                [nonnull, Outcome::Held, Outcome::Held, disjoint],
                            );
                        }
                    }
                }
                (
                    ParameterType::Pointer {
                        pointee,
                        access,
                        nullable,
                    },
                    RawArgument::Pointer { address },
                ) => {
                    if *address == 0 {
                        let nonnull = if *nullable {
                            na("None: a null Option[T]")
                        } else {
                            Outcome::Violated(ValidationError::NullPointer)
                        };
                        four(&mut report, [nonnull, na("null"), na("null"), na("null")]);
                        continue;
                    }
                    let nonnull = if *nullable {
                        na("Option[T] admits null")
                    } else {
                        Outcome::Held
                    };
                    let alignment = pointee.alignment();
                    if !address.is_multiple_of(alignment as u64) {
                        let e = ValidationError::MisalignedPointer { alignment };
                        four(
                            &mut report,
                            [
                                nonnull,
                                Outcome::Violated(e),
                                na("misaligned"),
                                na("misaligned"),
                            ],
                        );
                        continue;
                    }
                    // Pointee sizes are at most 1 MiB, so size - 1 fits.
                    match address.checked_add(pointee.size() as u64 - 1) {
                        None => four(
                            &mut report,
                            [
                                nonnull,
                                Outcome::Held,
                                Outcome::Violated(ValidationError::PointerRangeOverflow),
                                na("out of bounds"),
                            ],
                        ),
                        Some(last) => {
                            let b = Borrow {
                                argument: i,
                                first: *address,
                                last,
                                writes: access.writes(),
                            };
                            let disjoint = disjoint(&borrows, &b);
                            borrows.push(b);
                            four(
                                &mut report,
                                [nonnull, Outcome::Held, Outcome::Held, disjoint],
                            );
                        }
                    }
                }
                _ => report.push(
                    at,
                    name,
                    ArgumentShape,
                    Outcome::Violated(ValidationError::ArgumentKind),
                ),
            }
        }
        (report, borrows)
    }
}

/// Overlap against every earlier borrow when either side is written.
fn disjoint(earlier: &[Borrow], b: &Borrow) -> Outcome {
    for o in earlier {
        if b.first <= o.last && o.first <= b.last && (b.writes || o.writes) {
            return Outcome::Violated(ValidationError::OverlappingSlices { other: o.argument });
        }
    }
    Outcome::Held
}

fn guard(g: &GuardObservation) -> Outcome {
    if g.before.is_empty() || g.before.len() != g.after.len() {
        return Outcome::Unchecked("malformed guard observation: empty, or lengths differ");
    }
    if g.before.len() > MAX_GUARD_BYTES {
        return Outcome::Unchecked("guard observation exceeds 4096 bytes");
    }
    match g.before.iter().zip(&g.after).position(|(a, b)| a != b) {
        None => Outcome::Held,
        Some(first_changed) => {
            Outcome::Violated(ValidationError::GuardOverwritten { first_changed })
        }
    }
}
