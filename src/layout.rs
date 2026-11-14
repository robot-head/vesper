//! Concrete, eight-byte-aligned value layouts used by native code and its ABI.
//! Enums use a tag followed by the largest inline variant payload. Owned data
//! has no hidden reference counting or tracing header.
use crate::{ast::*, check::World};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct Layout {
    pub ty: Type,
    pub size: u32,
    pub kind: LayoutKind,
}
#[derive(Debug, Clone)]
pub enum LayoutKind {
    Scalar,
    Product(Vec<LayoutField>),
    Enum(Vec<LayoutVariant>),
}
#[derive(Debug, Clone)]
pub struct LayoutField {
    pub name: String,
    pub ty: Type,
    pub offset: i32,
    pub layout: Box<Layout>,
}
#[derive(Debug, Clone)]
pub struct LayoutVariant {
    pub name: String,
    pub fields: Vec<LayoutField>,
}
impl Layout {
    pub fn of(world: &World, ty: &Type) -> Result<Self, String> {
        Self::build(
            world,
            &crate::numeric::default_type(ty),
            &mut BTreeSet::new(),
            0,
        )
    }
    fn build(
        world: &World,
        ty: &Type,
        seen: &mut BTreeSet<String>,
        depth: usize,
    ) -> Result<Self, String> {
        if depth > 128 {
            return Err(
                "value layout exceeds 128 levels; use an owning indirection for recursive data"
                    .into(),
            );
        }
        if matches!(ty, Type::IntLiteral(_) | Type::Function { .. })
            || matches!(
                ty.name(),
                "u8" | "u32" | "u64" | "i64" | "f64" | "bool" | "Unit" | "Never"
            )
            || crate::regions::is_handle_type(ty)
            || *ty == Type::unit()
        {
            return Ok(Self {
                ty: ty.clone(),
                size: 8,
                kind: LayoutKind::Scalar,
            });
        }
        if let Type::Tuple(ts) = ty {
            let mut offset = 0;
            let mut fields = vec![];
            for (i, t) in ts.iter().enumerate() {
                let l = Self::build(world, t, seen, depth + 1)?;
                fields.push(LayoutField {
                    name: i.to_string(),
                    ty: t.clone(),
                    offset,
                    layout: Box::new(l.clone()),
                });
                offset = offset
                    .checked_add(l.size as i32)
                    .ok_or("value layout is too large")?;
            }
            return Ok(Self {
                ty: ty.clone(),
                size: (offset as u32).max(8),
                kind: LayoutKind::Product(fields),
            });
        }
        // Vectors and masks (§14.3, lane 28) are their packed lane bytes.
        if let Some(words) = crate::vector::layout_words(ty) {
            let word = Self {
                ty: Type::named("u64"),
                size: 8,
                kind: LayoutKind::Scalar,
            };
            return Ok(Self {
                ty: ty.clone(),
                size: words * 8,
                kind: LayoutKind::Product(
                    (0..words)
                        .map(|i| LayoutField {
                            name: format!("${i}"),
                            ty: word.ty.clone(),
                            offset: (i * 8) as i32,
                            layout: Box::new(word.clone()),
                        })
                        .collect(),
                ),
            });
        }
        // Source strings originate from literals. A closed module therefore
        // admits a finite inline representation without a heap owner or GC.
        if ty.name() == "String" {
            let words = world.string_capacity.div_ceil(8);
            let size = u32::try_from(
                words
                    .checked_add(1)
                    .and_then(|n| n.checked_mul(8))
                    .ok_or("string layout is too large")?,
            )
            .map_err(|_| "string layout is too large")?;
            let word = Self {
                ty: Type::named("u64"),
                size: 8,
                kind: LayoutKind::Scalar,
            };
            return Ok(Self {
                ty: ty.clone(),
                size,
                kind: LayoutKind::Product(
                    (0..=words)
                        .map(|i| LayoutField {
                            name: format!("${i}"),
                            ty: word.ty.clone(),
                            offset: (i * 8) as i32,
                            layout: Box::new(word.clone()),
                        })
                        .collect(),
                ),
            });
        }
        let Type::Named(n, args) = ty else {
            return Err(format!("native layout is not yet defined for {ty}"));
        };
        let a = world
            .adts
            .get(n)
            .ok_or_else(|| format!("native layout is not defined for {ty}"))?;
        if a.generics.len() != args.len() {
            return Err(format!("unresolved type arguments in layout of {ty}"));
        }
        let key = ty.to_string();
        if !seen.insert(key.clone()) {
            return Err(format!(
                "recursive inline layout of {ty}; an owning indirection is required"
            ));
        }
        let vars = a
            .generics
            .iter()
            .zip(args)
            .map(|(g, t)| (g.name.clone(), t.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut variants = vec![];
        let mut size = 8;
        for v in &a.variants {
            let mut offset = if a.is_enum { 8 } else { 0 };
            let mut fields = vec![];
            for f in &v.fields {
                let t = substitute(&f.ty, &vars);
                let l = Self::build(world, &t, seen, depth + 1)?;
                fields.push(LayoutField {
                    name: f.name.clone(),
                    ty: t,
                    offset,
                    layout: Box::new(l.clone()),
                });
                offset = offset
                    .checked_add(l.size as i32)
                    .ok_or("value layout is too large")?;
            }
            size = size.max(offset as u32);
            variants.push(LayoutVariant {
                name: v.name.clone(),
                fields,
            });
        }
        seen.remove(&key);
        Ok(Self {
            ty: ty.clone(),
            size,
            kind: if a.is_enum {
                LayoutKind::Enum(variants)
            } else {
                LayoutKind::Product(variants.remove(0).fields)
            },
        })
    }
    pub fn aggregate(&self) -> bool {
        !matches!(self.kind, LayoutKind::Scalar)
    }
    pub fn field(&self, name: &str) -> Option<&LayoutField> {
        if let LayoutKind::Product(fs) = &self.kind {
            fs.iter().find(|f| f.name == name)
        } else {
            None
        }
    }
    pub fn variant(&self, name: &str) -> Option<(usize, &[LayoutField])> {
        match &self.kind {
            LayoutKind::Product(fs) => Some((0, fs)),
            LayoutKind::Enum(vs) => vs
                .iter()
                .enumerate()
                .find(|(_, v)| Some(v.name.as_str()) == name.rsplit("::").next())
                .map(|(i, v)| (i, v.fields.as_slice())),
            _ => None,
        }
    }
}

pub fn decode(world: &World, ty: &Type, bytes: &[u8]) -> Result<crate::runtime::Value, String> {
    use crate::runtime::Value;
    let layout = Layout::of(world, ty)?;
    if bytes.len() < layout.size as usize {
        return Err("native result buffer is too short".into());
    }
    let word = || u64::from_ne_bytes(bytes[..8].try_into().unwrap());
    if let Some(value) = crate::vector::decode(ty, bytes) {
        return Ok(value);
    }
    if ty.name() == "String" {
        let length = usize::try_from(word()).map_err(|_| "invalid native string length")?;
        if length > world.string_capacity {
            return Err("invalid native string length".into());
        }
        return String::from_utf8(bytes[8..8 + length].to_vec())
            .map(Value::String)
            .map_err(|_| "invalid native UTF-8 string".into());
    }
    match &layout.kind {
        LayoutKind::Scalar => Ok(match ty.name() {
            "f64" => Value::Float(f64::from_bits(word())),
            "bool" => Value::Bool(word() != 0),
            "u8" => Value::UInt(word() & 255, 8),
            "u32" => Value::UInt(word() & u32::MAX as u64, 32),
            "u64" => Value::UInt(word(), 64),
            _ => {
                if *ty == Type::unit() || ty.name() == "Unit" {
                    Value::Tuple(vec![])
                } else {
                    Value::Int(word() as i64)
                }
            }
        }),
        LayoutKind::Product(fields) => {
            let mut values = BTreeMap::new();
            for f in fields {
                values.insert(
                    f.name.clone(),
                    decode(world, &f.ty, &bytes[f.offset as usize..])?,
                );
            }
            Ok(if matches!(ty, Type::Tuple(_)) {
                Value::Tuple(
                    fields
                        .iter()
                        .map(|f| values.remove(&f.name).unwrap())
                        .collect(),
                )
            } else {
                Value::Data {
                    ty: ty.name().into(),
                    constructor: ty.name().into(),
                    fields: values,
                    resource_identity: None,
                }
            })
        }
        LayoutKind::Enum(variants) => {
            let v = variants
                .get(word() as usize)
                .ok_or("invalid native enum discriminant")?;
            let mut fields = BTreeMap::new();
            for f in &v.fields {
                fields.insert(
                    f.name.clone(),
                    decode(world, &f.ty, &bytes[f.offset as usize..])?,
                );
            }
            Ok(Value::Data {
                ty: ty.name().into(),
                constructor: v.name.clone(),
                fields,
                resource_identity: None,
            })
        }
    }
}
