//! Concrete ADT modal-kind calculation (manuscript §3.2, `[K-Adt]`).
//!
//! Callers supply already-resolved field kinds for one instantiation. This
//! module does not resolve types or enforce the result in the source checker.

use crate::mode_vec::ModeVec;

/// A field's intrinsic kind and its optional explicit modality cap.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FieldKind {
    pub intrinsic: ModeVec,
    /// An omitted override has the lattice top as its cap.
    pub cap: Option<ModeVec>,
}

/// An explicit field cap cannot be satisfied by the field's intrinsic kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidFieldKind {
    /// Zero-based index in the supplied field slice.
    pub field_index: usize,
    pub intrinsic: ModeVec,
    pub cap: ModeVec,
}

/// Compute the kind of an instantiated ADT, rejecting the first invalid cap.
///
/// This implements `declared ∨ ⋁ (intrinsic ∧ cap)` with omitted caps
/// treated as top, after requiring each `intrinsic ≤ cap`.
pub fn adt_kind(declared: ModeVec, fields: &[FieldKind]) -> Result<ModeVec, InvalidFieldKind> {
    let mut result = declared;
    for (field_index, field) in fields.iter().enumerate() {
        let cap = field.cap.unwrap_or(ModeVec::TOP);
        if !field.intrinsic.leq(cap) {
            return Err(InvalidFieldKind {
                field_index,
                intrinsic: field.intrinsic,
                cap,
            });
        }
        result = result.join(field.intrinsic.meet(cap));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode_vec::{Contention, Order, Portability, Usage};

    #[test]
    fn empty_and_polymorphic_fields_respect_declaration_floor() {
        assert_eq!(
            adt_kind(ModeVec::NOMINAL_DEFAULT, &[]),
            Ok(ModeVec::NOMINAL_DEFAULT)
        );
        // A copy-declared Option[T] crosses to its instantiated T's kind.
        for bits in 0..32 {
            if let Some(kind) = ModeVec::from_bits(bits) {
                assert_eq!(
                    adt_kind(
                        ModeVec::BOTTOM,
                        &[FieldKind {
                            intrinsic: kind,
                            cap: None
                        }]
                    ),
                    Ok(kind)
                );
            }
        }
    }

    #[test]
    fn field_caps_are_validated_componentwise_before_join() {
        let pinned = ModeVec::new(
            Usage::Copy,
            Portability::Pinned,
            Contention::Uncontended,
            Order::Unordered,
        );
        let linear = ModeVec::new(
            Usage::Linear,
            Portability::Portable,
            Contention::Uncontended,
            Order::Unordered,
        );
        let fields = [
            FieldKind {
                intrinsic: pinned,
                cap: None,
            },
            FieldKind {
                intrinsic: linear,
                cap: Some(ModeVec::NOMINAL_DEFAULT),
            },
        ];
        assert_eq!(
            adt_kind(ModeVec::BOTTOM, &fields),
            Err(InvalidFieldKind {
                field_index: 1,
                intrinsic: linear,
                cap: ModeVec::NOMINAL_DEFAULT
            })
        );
        assert_eq!(
            adt_kind(
                ModeVec::BOTTOM,
                &[FieldKind {
                    intrinsic: pinned,
                    cap: Some(linear)
                }]
            ),
            Err(InvalidFieldKind {
                field_index: 0,
                intrinsic: pinned,
                cap: linear
            })
        );
        assert_eq!(
            adt_kind(
                ModeVec::BOTTOM,
                &[
                    fields[0],
                    FieldKind {
                        intrinsic: linear,
                        cap: Some(ModeVec::TOP)
                    }
                ]
            ),
            Ok(pinned.join(linear))
        );
    }
}
