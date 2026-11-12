use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    Width { bits: u8 },
    InvalidBool,
    InvalidEnum { name: String },
    NullAddress,
    MisalignedAddress { alignment: u8 },
    NonzeroVoid,
    ForeignErrorCode(u64),
    ErrorCodeBudget,
    ArgumentCount,
    ArgumentKind,
    SliceRangeOverflow,
    SliceTooLarge,
    OverlappingSlices { other: usize },
}
impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ValidationError {}
/// Numerically checked address only. This is deliberately not a pointer,
/// reference, allocation identity, ownership capability or validity proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckedAddress {
    address: u64,
    pointee: Scalar,
    nullable: bool,
}
impl CheckedAddress {
    pub fn address(self) -> u64 {
        self.address
    }
    pub fn pointee(self) -> Scalar {
        self.pointee
    }
    pub fn nullable(self) -> bool {
        self.nullable
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidatedValue {
    Unit,
    Unsigned(u64),
    Signed(i64),
    Bool(bool),
    Enum { name: String, discriminant: u64 },
    Address(CheckedAddress),
}
/// All raw returns use total-bit Rust/C carriers. Opaque owner `O` is retained
/// through both successful validation and every validation failure.
#[must_use = "retain or explicitly recover the foreign value and its owner"]
#[derive(Debug)]
pub struct ForeignValue<O> {
    bits: u64,
    contract: ValueType,
    owner: O,
}
impl<O> ForeignValue<O> {
    pub fn raw_bits(&self) -> u64 {
        self.bits
    }
    pub fn owner(&self) -> &O {
        &self.owner
    }
    pub fn into_parts(self) -> (u64, O) {
        (self.bits, self.owner)
    }
    pub fn validate(self) -> Result<ValidatedReturn<O>, InvalidForeignValue<O>> {
        match validate(&self.contract, self.bits) {
            Ok(value) => Ok(ValidatedReturn {
                value,
                owner: self.owner,
            }),
            Err(error) => Err(InvalidForeignValue { error, value: self }),
        }
    }
    /// Refine a valid carrier while rejecting an explicit bounded list of
    /// library error sentinel bit patterns. The caller supplies this policy.
    pub fn validate_non_error(
        self,
        codes: &[u64],
    ) -> Result<ValidatedReturn<O>, InvalidForeignValue<O>> {
        if codes.len() > 256 {
            return Err(InvalidForeignValue {
                error: ValidationError::ErrorCodeBudget,
                value: self,
            });
        }
        if codes.contains(&self.bits) {
            return Err(InvalidForeignValue {
                error: ValidationError::ForeignErrorCode(self.bits),
                value: self,
            });
        }
        self.validate()
    }
}
#[must_use = "validation failure retains the foreign value and its owner"]
#[derive(Debug)]
pub struct InvalidForeignValue<O> {
    error: ValidationError,
    value: ForeignValue<O>,
}
impl<O> InvalidForeignValue<O> {
    pub fn error(&self) -> &ValidationError {
        &self.error
    }
    pub fn into_parts(self) -> (ValidationError, ForeignValue<O>) {
        (self.error, self.value)
    }
}
#[must_use = "validation preserves the owner; recover it explicitly"]
#[derive(Debug)]
pub struct ValidatedReturn<O> {
    value: ValidatedValue,
    owner: O,
}
impl<O> ValidatedReturn<O> {
    pub fn value(&self) -> &ValidatedValue {
        &self.value
    }
    pub fn owner(&self) -> &O {
        &self.owner
    }
    pub fn into_parts(self) -> (ValidatedValue, O) {
        (self.value, self.owner)
    }
}
fn scalar(s: Scalar, bits: u64) -> Result<ValidatedValue, ValidationError> {
    let width = s.bits();
    if width < 64 && bits >= 1u64 << width {
        return Err(ValidationError::Width { bits: width });
    }
    Ok(if s.signed() {
        ValidatedValue::Signed(((bits << (64 - width)) as i64) >> (64 - width))
    } else {
        ValidatedValue::Unsigned(bits)
    })
}
fn address(bits: u64, pointee: Scalar, nullable: bool) -> Result<CheckedAddress, ValidationError> {
    let alignment = pointee.bits() / 8;
    if bits == 0 && !nullable {
        return Err(ValidationError::NullAddress);
    }
    if !bits.is_multiple_of(alignment as u64) {
        return Err(ValidationError::MisalignedAddress { alignment });
    }
    Ok(CheckedAddress {
        address: bits,
        pointee,
        nullable,
    })
}
fn validate(contract: &ValueType, bits: u64) -> Result<ValidatedValue, ValidationError> {
    Ok(match contract {
        ValueType::Unit => {
            if bits != 0 {
                return Err(ValidationError::NonzeroVoid);
            }
            ValidatedValue::Unit
        }
        ValueType::Scalar(s) => scalar(*s, bits)?,
        ValueType::Bool => match bits {
            0 => ValidatedValue::Bool(false),
            1 => ValidatedValue::Bool(true),
            _ => return Err(ValidationError::InvalidBool),
        },
        ValueType::Enum(e) => {
            scalar(e.scalar(), bits)?;
            if !e.variants().iter().any(|(_, value)| *value == bits) {
                return Err(ValidationError::InvalidEnum {
                    name: e.name().to_owned(),
                });
            }
            ValidatedValue::Enum {
                name: e.name().to_owned(),
                discriminant: bits,
            }
        }
        ValueType::Address { pointee, nullable } => {
            ValidatedValue::Address(address(bits, *pointee, *nullable)?)
        }
    })
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawArgument {
    /// Exact-width bit representation, never a prevalidated bool/enum.
    Bits(u64),
    /// Element count, not byte count. Numeric checks do not prove an allocation
    /// exists; the unsafe caller remains responsible for valid borrowed memory.
    Slice { address: u64, length: u64 },
}
#[must_use = "failed preparation retains arguments and their owner"]
#[derive(Debug)]
pub struct CallFailure<O> {
    error: ValidationError,
    argument: Option<usize>,
    arguments: Vec<RawArgument>,
    owner: O,
}
impl<O> CallFailure<O> {
    pub fn error(&self) -> &ValidationError {
        &self.error
    }
    pub fn argument(&self) -> Option<usize> {
        self.argument
    }
    pub fn into_parts(self) -> (ValidationError, Option<usize>, Vec<RawArgument>, O) {
        (self.error, self.argument, self.arguments, self.owner)
    }
}
/// Validated scalar bit carriers and numerical slice ranges. This object does
/// not execute foreign code or establish provenance, permission or `nocapture`.
#[must_use = "prepared arguments retain their owner"]
#[derive(Debug)]
pub struct PreparedCall<O> {
    arguments: Vec<(AbiType, u64)>,
    owner: O,
}
impl<O> PreparedCall<O> {
    pub fn arguments(&self) -> &[(AbiType, u64)] {
        &self.arguments
    }
    pub fn owner(&self) -> &O {
        &self.owner
    }
    pub fn into_parts(self) -> (Vec<(AbiType, u64)>, O) {
        (self.arguments, self.owner)
    }
}
impl FunctionDescriptor {
    /// Capture the exact-width raw return representation. For signed narrow
    /// integers callers first cast through the unsigned carrier of that width.
    /// Void has bits=0. No foreign bool/enum is constructed before validation.
    pub fn capture_return<O>(&self, bits: u64, owner: O) -> ForeignValue<O> {
        ForeignValue {
            bits,
            contract: self.result.clone(),
            owner,
        }
    }
    pub fn prepare_call<O>(
        &self,
        arguments: Vec<RawArgument>,
        owner: O,
    ) -> Result<PreparedCall<O>, CallFailure<O>> {
        if let Err((argument, error)) = self.validate_arguments(&arguments) {
            return Err(CallFailure {
                error,
                argument,
                arguments,
                owner,
            });
        }
        let mut raw = vec![];
        for (p, a) in self.parameters.iter().zip(arguments) {
            match (&p.ty, a) {
                (ParameterType::Value(t), RawArgument::Bits(bits)) => {
                    raw.push((carrier(t).expect("non-unit parameters"), bits))
                }
                (ParameterType::Slice { .. }, RawArgument::Slice { address, length }) => {
                    raw.push((AbiType::Pointer, address));
                    raw.push((AbiType::Integer(Scalar::Usize), length));
                }
                _ => unreachable!("arguments validated before allocation"),
            }
        }
        Ok(PreparedCall {
            arguments: raw,
            owner,
        })
    }
    fn validate_arguments(
        &self,
        args: &[RawArgument],
    ) -> Result<(), (Option<usize>, ValidationError)> {
        if args.len() != self.parameters.len() {
            return Err((None, ValidationError::ArgumentCount));
        }
        // At most the configured parameter cap (hard limit 32) ranges.
        let mut ranges: Vec<(usize, u64, u64, Access)> = vec![];
        for (i, (p, a)) in self.parameters.iter().zip(args).enumerate() {
            match (&p.ty, a) {
                (ParameterType::Value(t), RawArgument::Bits(bits)) => {
                    validate(t, *bits).map_err(|e| (Some(i), e))?;
                }
                (
                    ParameterType::Slice { element, access },
                    RawArgument::Slice {
                        address: base,
                        length,
                    },
                ) => {
                    address(*base, *element, *length == 0).map_err(|e| (Some(i), e))?;
                    let bytes = length
                        .checked_mul((element.bits() / 8) as u64)
                        .ok_or((Some(i), ValidationError::SliceRangeOverflow))?;
                    if bytes > i64::MAX as u64 {
                        return Err((Some(i), ValidationError::SliceTooLarge));
                    }
                    if bytes > 0 {
                        let last = base
                            .checked_add(bytes - 1)
                            .ok_or((Some(i), ValidationError::SliceRangeOverflow))?;
                        for (other, first, end, other_access) in &ranges {
                            if *base <= *end
                                && *first <= last
                                && (*access == Access::Inout || *other_access == Access::Inout)
                            {
                                return Err((
                                    Some(i),
                                    ValidationError::OverlappingSlices { other: *other },
                                ));
                            }
                        }
                        ranges.push((i, *base, last, *access));
                    }
                }
                _ => return Err((Some(i), ValidationError::ArgumentKind)),
            }
        }
        Ok(())
    }
}
