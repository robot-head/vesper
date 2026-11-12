//! The integer semantics shared by checking, execution, optimization and codegen.
use crate::ast::Type;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegerKind {
    U8,
    U32,
    U64,
    I64,
}
impl IntegerKind {
    pub fn from_type(t: &Type) -> Option<Self> {
        Some(match t.name() {
            "u8" => Self::U8,
            "u32" => Self::U32,
            "u64" => Self::U64,
            "i64" => Self::I64,
            _ => {
                if let Type::IntLiteral(n) = t {
                    return Some(Self::default_for(*n));
                } else {
                    return None;
                }
            }
        })
    }
    pub fn default_for(n: i128) -> Self {
        if n > i64::MAX as i128 {
            Self::U64
        } else {
            Self::I64
        }
    }
    pub fn ty(self) -> Type {
        Type::named(match self {
            Self::U8 => "u8",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::I64 => "i64",
        })
    }
    pub fn bits(self) -> u32 {
        match self {
            Self::U8 => 8,
            Self::U32 => 32,
            _ => 64,
        }
    }
    pub fn unsigned(self) -> bool {
        self != Self::I64
    }
    pub fn fits(self, n: i128) -> bool {
        match self {
            Self::I64 => (i64::MIN as i128..=i64::MAX as i128).contains(&n),
            _ => (0..=(1i128 << self.bits()) - 1).contains(&n),
        }
    }
    pub fn wrap(self, n: i128) -> i128 {
        if self == Self::I64 {
            n as i64 as i128
        } else {
            (n as u64 & self.mask()) as i128
        }
    }
    pub fn mask(self) -> u64 {
        if self.bits() == 64 {
            u64::MAX
        } else {
            (1u64 << self.bits()) - 1
        }
    }
}
pub fn default_type(t: &Type) -> Type {
    match t {
        Type::IntLiteral(n) => IntegerKind::default_for(*n).ty(),
        Type::Named(n, ts) => Type::Named(n.clone(), ts.iter().map(default_type).collect()),
        Type::Tuple(ts) => Type::Tuple(ts.iter().map(default_type).collect()),
        Type::Function {
            params,
            result,
            effects,
            mode,
        } => Type::Function {
            params: params.iter().map(|(p, t)| (*p, default_type(t))).collect(),
            result: Box::new(default_type(result)),
            effects: crate::ast::Effects {
                instances: effects
                    .instances
                    .iter()
                    .map(|e| crate::ast::Effect {
                        label: e.label.clone(),
                        name: e.name.clone(),
                        args: e.args.iter().map(default_type).collect(),
                    })
                    .collect(),
                tail: effects.tail.clone(),
            },
            mode: *mode,
        },
        _ => t.clone(),
    }
}
