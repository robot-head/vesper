//! The four-axis modal product lattice from manuscript §3.1.
//!
//! This is a value-level representation of the ordering, not a checker or a
//! claim that source programs are enforced against all four axes.

/// Structural usage restriction, in increasing order.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[repr(u8)]
pub enum Usage {
    Copy = 0,
    Affine = 1,
    Linear = 2,
}

/// Cross-shard transfer restriction.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[repr(u8)]
pub enum Portability {
    Portable = 0,
    Pinned = 1,
}

/// Shared-access contention restriction.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[repr(u8)]
pub enum Contention {
    Uncontended = 0,
    Contended = 1,
}

/// Obligation discharge order restriction.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[repr(u8)]
pub enum Order {
    Unordered = 0,
    Lifo = 1,
}

/// One byte: usage in bits 0–1, then portability, contention and order.
/// Reserved bits and the unused usage encoding are rejected by `from_bits`.
/// The numeric byte order is not the lattice order; use [`Self::leq`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ModeVec(u8);

impl ModeVec {
    pub const BOTTOM: Self = Self::new(
        Usage::Copy,
        Portability::Portable,
        Contention::Uncontended,
        Order::Unordered,
    );
    pub const TOP: Self = Self::new(
        Usage::Linear,
        Portability::Pinned,
        Contention::Contended,
        Order::Lifo,
    );
    pub const NOMINAL_DEFAULT: Self = Self::new(
        Usage::Affine,
        Portability::Portable,
        Contention::Uncontended,
        Order::Unordered,
    );

    pub const fn new(
        usage: Usage,
        portability: Portability,
        contention: Contention,
        order: Order,
    ) -> Self {
        Self(usage as u8 | (portability as u8) << 2 | (contention as u8) << 3 | (order as u8) << 4)
    }

    pub const fn from_bits(bits: u8) -> Option<Self> {
        if bits & !0x1f != 0 || bits & 3 == 3 {
            None
        } else {
            Some(Self(bits))
        }
    }

    pub const fn bits(self) -> u8 {
        self.0
    }
    pub const fn usage(self) -> Usage {
        match self.0 & 3 {
            0 => Usage::Copy,
            1 => Usage::Affine,
            _ => Usage::Linear,
        }
    }
    pub const fn portability(self) -> Portability {
        if self.0 & 4 == 0 {
            Portability::Portable
        } else {
            Portability::Pinned
        }
    }
    pub const fn contention(self) -> Contention {
        if self.0 & 8 == 0 {
            Contention::Uncontended
        } else {
            Contention::Contended
        }
    }
    pub const fn order(self) -> Order {
        if self.0 & 16 == 0 {
            Order::Unordered
        } else {
            Order::Lifo
        }
    }

    /// Componentwise order: each axis is no more restrictive than `other`.
    pub const fn leq(self, other: Self) -> bool {
        (self.0 & 3) <= (other.0 & 3) && (self.0 & 0x1c) & !(other.0 & 0x1c) == 0
    }

    /// Least upper bound, taking the stricter value on every axis.
    pub const fn join(self, other: Self) -> Self {
        let a = self.0 & 3;
        let b = other.0 & 3;
        Self((if a > b { a } else { b }) | ((self.0 | other.0) & 0x1c))
    }

    /// Greatest lower bound, taking the less restrictive value on every axis.
    pub const fn meet(self, other: Self) -> Self {
        let a = self.0 & 3;
        let b = other.0 & 3;
        Self((if a < b { a } else { b }) | ((self.0 & other.0) & 0x1c))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodings_and_axis_roundtrips() {
        let mut count = 0;
        for bits in u8::MIN..=u8::MAX {
            let value = ModeVec::from_bits(bits);
            assert_eq!(value.is_some(), bits < 32 && bits % 4 != 3);
            if let Some(value) = value {
                count += 1;
                assert_eq!(value.bits(), bits);
                assert_eq!(
                    ModeVec::new(
                        value.usage(),
                        value.portability(),
                        value.contention(),
                        value.order()
                    ),
                    value
                );
            }
        }
        assert_eq!(count, 24);
        assert_eq!(ModeVec::NOMINAL_DEFAULT.usage(), Usage::Affine);
        assert_eq!(std::mem::size_of::<ModeVec>(), 1);
    }

    #[test]
    fn product_order_and_bounds_are_exact() {
        let all: Vec<_> = (0..32).filter_map(ModeVec::from_bits).collect();
        for &a in &all {
            assert!(ModeVec::BOTTOM.leq(a));
            assert!(a.leq(ModeVec::TOP));
            for &b in &all {
                let expected = a.usage() <= b.usage()
                    && a.portability() <= b.portability()
                    && a.contention() <= b.contention()
                    && a.order() <= b.order();
                assert_eq!(a.leq(b), expected);
                let join = a.join(b);
                let meet = a.meet(b);
                assert!(a.leq(join) && b.leq(join));
                assert!(meet.leq(a) && meet.leq(b));
                for &c in &all {
                    if a.leq(c) && b.leq(c) {
                        assert!(join.leq(c));
                    }
                    if c.leq(a) && c.leq(b) {
                        assert!(c.leq(meet));
                    }
                }
            }
        }
        // Integer comparison cannot substitute for componentwise order.
        let pinned = ModeVec::new(
            Usage::Copy,
            Portability::Pinned,
            Contention::Uncontended,
            Order::Unordered,
        );
        let affine = ModeVec::NOMINAL_DEFAULT;
        assert!(affine.bits() < pinned.bits());
        assert!(!affine.leq(pinned) && !pinned.leq(affine));
    }
}
