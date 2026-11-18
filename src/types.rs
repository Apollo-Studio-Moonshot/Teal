//! Semantic types.
//!
//! Types are interned as plain enums with `String` names. Canonicalization is
//! structural (`Display` doubles as the canonical string), which is what makes
//! instance keys and `.tli` digests stable across packages (DESIGN.md §4.3).

use std::collections::BTreeMap;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Type {
    Error,
    Unit,
    Never,
    Bool,
    Char,
    Str,
    Int(IntTy),
    Float(FloatTy),
    Named {
        name: String,
        args: Vec<Type>,
    },
    Param(String),
    SelfType,
    Ref {
        inner: Box<Type>,
        mutable: bool,
    },
    Slice(Box<Type>),
    Tuple(Vec<Type>),
    Fn {
        params: Vec<Type>,
        ret: Box<Type>,
    },
    Dyn {
        name: String,
        args: Vec<Type>,
        assoc: Vec<(String, Type)>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IntTy {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    Usize,
    Isize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FloatTy {
    F32,
    F64,
}

impl IntTy {
    pub fn name(self) -> &'static str {
        match self {
            IntTy::I8 => "i8",
            IntTy::I16 => "i16",
            IntTy::I32 => "i32",
            IntTy::I64 => "i64",
            IntTy::U8 => "u8",
            IntTy::U16 => "u16",
            IntTy::U32 => "u32",
            IntTy::U64 => "u64",
            IntTy::Usize => "usize",
            IntTy::Isize => "isize",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "i8" => IntTy::I8,
            "i16" => IntTy::I16,
            "i32" => IntTy::I32,
            "i64" => IntTy::I64,
            "u8" => IntTy::U8,
            "u16" => IntTy::U16,
            "u32" => IntTy::U32,
            "u64" => IntTy::U64,
            "usize" => IntTy::Usize,
            "isize" => IntTy::Isize,
            _ => return None,
        })
    }

    pub fn is_signed(self) -> bool {
        matches!(
            self,
            IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::Isize
        )
    }
}

impl FloatTy {
    pub fn name(self) -> &'static str {
        match self {
            FloatTy::F32 => "f32",
            FloatTy::F64 => "f64",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "f32" => Some(FloatTy::F32),
            "f64" => Some(FloatTy::F64),
            _ => None,
        }
    }
}

impl Type {
    pub fn unit() -> Self {
        Type::Unit
    }

    pub fn error() -> Self {
        Type::Error
    }

    pub fn named(name: impl Into<String>) -> Self {
        Type::Named {
            name: name.into(),
            args: Vec::new(),
        }
    }

    pub fn generic(name: impl Into<String>, args: Vec<Type>) -> Self {
        Type::Named {
            name: name.into(),
            args,
        }
    }

    pub fn primitive(name: &str) -> Option<Type> {
        if let Some(int) = IntTy::from_name(name) {
            return Some(Type::Int(int));
        }
        if let Some(float) = FloatTy::from_name(name) {
            return Some(Type::Float(float));
        }
        Some(match name {
            "bool" => Type::Bool,
            "char" => Type::Char,
            "string" => Type::Str,
            _ => return None,
        })
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Type::Error)
    }

    pub fn is_never(&self) -> bool {
        matches!(self, Type::Never)
    }

    pub fn is_unit(&self) -> bool {
        matches!(self, Type::Unit)
    }

    pub fn is_integer(&self) -> bool {
        matches!(self, Type::Int(_))
    }

    pub fn is_float(&self) -> bool {
        matches!(self, Type::Float(_))
    }

    pub fn is_numeric(&self) -> bool {
        matches!(self, Type::Int(_) | Type::Float(_))
    }

    pub fn is_bool(&self) -> bool {
        matches!(self, Type::Bool)
    }

    pub fn is_str(&self) -> bool {
        matches!(self, Type::Str)
    }

    pub fn is_ref(&self) -> bool {
        matches!(self, Type::Ref { .. })
    }

    pub fn is_param(&self) -> bool {
        matches!(self, Type::Param(_))
    }

    pub fn is_concrete(&self) -> bool {
        match self {
            Type::Param(_) | Type::SelfType | Type::Error => false,
            Type::Ref { inner, .. } | Type::Slice(inner) => inner.is_concrete(),
            Type::Tuple(items) => items.iter().all(Type::is_concrete),
            Type::Named { args, .. } => args.iter().all(Type::is_concrete),
            Type::Fn { params, ret } => params.iter().all(Type::is_concrete) && ret.is_concrete(),
            Type::Dyn { .. } => true,
            _ => true,
        }
    }

    pub fn deref(&self) -> &Type {
        match self {
            Type::Ref { inner, .. } => inner,
            other => other,
        }
    }

    pub fn as_ref_type(&self) -> Option<(&Type, bool)> {
        match self {
            Type::Ref { inner, mutable } => Some((inner, *mutable)),
            _ => None,
        }
    }

    /// The head constructor used for impl lookup. Generic arguments are
    /// deliberately dropped: `impl Eq for [T]` is keyed by `[]`.
    pub fn head(&self) -> String {
        match self {
            Type::Error => "<error>".into(),
            Type::Unit => "()".into(),
            Type::Never => "!".into(),
            Type::Bool => "bool".into(),
            Type::Char => "char".into(),
            Type::Str => "string".into(),
            Type::Int(int) => int.name().into(),
            Type::Float(float) => float.name().into(),
            Type::Named { name, .. } => name.clone(),
            Type::Param(name) => name.clone(),
            Type::SelfType => "Self".into(),
            Type::Ref { .. } => "&".into(),
            Type::Slice(_) => "[]".into(),
            Type::Tuple(items) => format!("({})", items.len()),
            Type::Fn { .. } => "fn".into(),
            Type::Dyn { name, .. } => format!("dyn {name}"),
        }
    }

    pub fn display(&self) -> String {
        self.to_string()
    }

    pub fn substitute(&self, subst: &BTreeMap<String, Type>) -> Type {
        match self {
            Type::Param(name) => subst.get(name).cloned().unwrap_or_else(|| self.clone()),
            Type::Named { name, args } => Type::Named {
                name: name.clone(),
                args: args.iter().map(|t| t.substitute(subst)).collect(),
            },
            Type::Ref { inner, mutable } => Type::Ref {
                inner: Box::new(inner.substitute(subst)),
                mutable: *mutable,
            },
            Type::Slice(inner) => Type::Slice(Box::new(inner.substitute(subst))),
            Type::Tuple(items) => Type::Tuple(items.iter().map(|t| t.substitute(subst)).collect()),
            Type::Fn { params, ret } => Type::Fn {
                params: params.iter().map(|t| t.substitute(subst)).collect(),
                ret: Box::new(ret.substitute(subst)),
            },
            Type::Dyn { name, args, assoc } => Type::Dyn {
                name: name.clone(),
                args: args.iter().map(|t| t.substitute(subst)).collect(),
                assoc: assoc
                    .iter()
                    .map(|(k, v)| (k.clone(), v.substitute(subst)))
                    .collect(),
            },
            _ => self.clone(),
        }
    }

    pub fn contains_param(&self) -> bool {
        match self {
            Type::Param(_) | Type::SelfType => true,
            Type::Ref { inner, .. } | Type::Slice(inner) => inner.contains_param(),
            Type::Named { args, .. } => args.iter().any(Type::contains_param),
            Type::Tuple(items) => items.iter().any(Type::contains_param),
            Type::Fn { params, ret } => {
                params.iter().any(Type::contains_param) || ret.contains_param()
            }
            Type::Dyn { args, assoc, .. } => {
                args.iter().any(Type::contains_param)
                    || assoc.iter().any(|(_, t)| t.contains_param())
            }
            _ => false,
        }
    }

    /// Strict assignability: Teal has no implicit conversions (DESIGN.md §3.9).
    /// `Error` and `Never` are wildcards for error recovery and divergence.
    pub fn assignable_to(&self, target: &Type) -> bool {
        if self.is_error() || target.is_error() || self.is_never() {
            return true;
        }
        if self == target {
            return true;
        }
        match (self, target) {
            (
                Type::Ref {
                    inner: from,
                    mutable: from_mut,
                },
                Type::Ref {
                    inner: to,
                    mutable: to_mut,
                },
            ) => {
                if *from_mut && !*to_mut {
                    return false;
                }
                from.assignable_to(to)
            }
            (Type::Named { name: a, args: aa }, Type::Named { name: b, args: ba })
                if a == b && aa.len() == ba.len() =>
            {
                aa.iter().zip(ba).all(|(x, y)| x.assignable_to(y))
            }
            (Type::Slice(a), Type::Slice(b)) => a.assignable_to(b),
            _ => false,
        }
    }

    /// Structural match used for impl-head lookup and method receiver checks.
    /// Type parameters in `pattern` match any concrete type.
    pub fn matches_pattern(&self, pattern: &Type, subst: &mut BTreeMap<String, Type>) -> bool {
        match pattern {
            Type::Param(name) => {
                if let Some(existing) = subst.get(name) {
                    existing == self
                } else {
                    subst.insert(name.clone(), self.clone());
                    true
                }
            }
            Type::Error => true,
            Type::Named {
                name: pname,
                args: pargs,
            } => match self {
                Type::Named { name, args } if name == pname && args.len() == pargs.len() => args
                    .iter()
                    .zip(pargs)
                    .all(|(a, p)| a.matches_pattern(p, subst)),
                _ => false,
            },
            Type::Ref {
                inner: pinner,
                mutable: pmut,
            } => match self {
                Type::Ref { inner, mutable } if mutable == pmut => {
                    inner.matches_pattern(pinner, subst)
                }
                _ => false,
            },
            Type::Slice(pinner) => match self {
                Type::Slice(inner) => inner.matches_pattern(pinner, subst),
                _ => false,
            },
            Type::Tuple(pitems) => match self {
                Type::Tuple(items) if items.len() == pitems.len() => items
                    .iter()
                    .zip(pitems)
                    .all(|(a, p)| a.matches_pattern(p, subst)),
                _ => false,
            },
            Type::Fn {
                params: pp,
                ret: pr,
            } => match self {
                Type::Fn { params, ret } if params.len() == pp.len() => {
                    params
                        .iter()
                        .zip(pp)
                        .all(|(a, p)| a.matches_pattern(p, subst))
                        && ret.matches_pattern(pr, subst)
                }
                _ => false,
            },
            other => self == other,
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Error => write!(f, "<error>"),
            Type::Unit => write!(f, "()"),
            Type::Never => write!(f, "!"),
            Type::Bool => write!(f, "bool"),
            Type::Char => write!(f, "char"),
            Type::Str => write!(f, "string"),
            Type::Int(int) => write!(f, "{}", int.name()),
            Type::Float(float) => write!(f, "{}", float.name()),
            Type::Named { name, args } => {
                write!(f, "{name}")?;
                if !args.is_empty() {
                    write!(
                        f,
                        "<{}>",
                        args.iter()
                            .map(|t| t.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )?;
                }
                Ok(())
            }
            Type::Param(name) => write!(f, "{name}"),
            Type::SelfType => write!(f, "Self"),
            Type::Ref { inner, mutable } => {
                if *mutable {
                    write!(f, "&mut {inner}")
                } else {
                    write!(f, "&{inner}")
                }
            }
            Type::Slice(inner) => write!(f, "[{inner}]"),
            Type::Tuple(items) => write!(
                f,
                "({})",
                items
                    .iter()
                    .map(|t| t.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Type::Fn { params, ret } => write!(
                f,
                "fn({}) -> {}",
                params
                    .iter()
                    .map(|t| t.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                ret
            ),
            Type::Dyn { name, args, assoc } => {
                write!(f, "dyn {name}")?;
                if !args.is_empty() || !assoc.is_empty() {
                    let mut parts: Vec<String> = args.iter().map(|t| t.to_string()).collect();
                    parts.extend(assoc.iter().map(|(k, v)| format!("{k} = {v}")));
                    write!(f, "<{}>", parts.join(", "))?;
                }
                Ok(())
            }
        }
    }
}

/// Parse a canonical type string back into a `Type`. Used when loading `.tli`
/// summaries: the summary format stores canonical strings, and this parser is
/// deliberately bounded and non-recursive-descent on untrusted lengths.
pub fn parse_type(text: &str) -> Option<Type> {
    let mut parser = TypeParser {
        chars: text.chars().collect(),
        pos: 0,
    };
    let ty = parser.parse()?;
    parser.skip_ws();
    if parser.pos == parser.chars.len() {
        Some(ty)
    } else {
        None
    }
}

struct TypeParser {
    chars: Vec<char>,
    pos: usize,
}

impl TypeParser {
    fn skip_ws(&mut self) {
        while self.pos < self.chars.len() && self.chars[self.pos].is_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        self.skip_ws();
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn parse(&mut self) -> Option<Type> {
        self.skip_ws();
        if self.eat('&') {
            let mutable = self.parse_word() == Some("mut".to_string());
            let inner = self.parse()?;
            return Some(Type::Ref {
                inner: Box::new(inner),
                mutable,
            });
        }
        if self.eat('[') {
            let inner = self.parse()?;
            if !self.eat(']') {
                return None;
            }
            return Some(Type::Slice(Box::new(inner)));
        }
        if self.eat('(') {
            self.skip_ws();
            if self.eat(')') {
                return Some(Type::Unit);
            }
            let mut items = Vec::new();
            loop {
                items.push(self.parse()?);
                self.skip_ws();
                if self.eat(',') {
                    self.skip_ws();
                    if self.peek() == Some(')') {
                        self.pos += 1;
                        break;
                    }
                    continue;
                }
                if !self.eat(')') {
                    return None;
                }
                break;
            }
            if items.len() == 1 {
                return Some(items.remove(0));
            }
            return Some(Type::Tuple(items));
        }
        if self.eat('!') {
            return Some(Type::Never);
        }
        if self.starts_with("dyn ") {
            self.pos += 4;
            let name = self.parse_path()?;
            let (args, assoc) = self.parse_args()?;
            return Some(Type::Dyn { name, args, assoc });
        }
        if self.starts_with("fn(") {
            self.pos += 3;
            let mut params = Vec::new();
            loop {
                self.skip_ws();
                if self.eat(')') {
                    break;
                }
                params.push(self.parse()?);
                self.skip_ws();
                if !self.eat(',') {
                    if !self.eat(')') {
                        return None;
                    }
                    break;
                }
            }
            self.skip_ws();
            let ret = if self.starts_with("->") {
                self.pos += 2;
                self.parse()?
            } else {
                Type::Unit
            };
            return Some(Type::Fn {
                params,
                ret: Box::new(ret),
            });
        }
        let name = self.parse_path()?;
        if let Some(primitive) = Type::primitive(&name) {
            return Some(primitive);
        }
        let (args, assoc) = self.parse_args()?;
        Some(Type::Named { name, args }).map(|ty| {
            let _ = &assoc;
            ty
        })
    }

    fn parse_path(&mut self) -> Option<String> {
        self.skip_ws();
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' || c == ':' || c == '<' || c == '>' || c == ' ' {
                if c == '<' || c == '>' {
                    break;
                }
                if c == ' ' {
                    break;
                }
                out.push(c);
                self.pos += 1;
            } else {
                break;
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    fn parse_args(&mut self) -> Option<(Vec<Type>, Vec<(String, Type)>)> {
        self.skip_ws();
        if !self.eat('<') {
            return Some((Vec::new(), Vec::new()));
        }
        let mut args = Vec::new();
        let mut assoc = Vec::new();
        loop {
            self.skip_ws();
            if self.eat('>') {
                break;
            }
            // Look ahead for `Name = Type`.
            let save = self.pos;
            let maybe_name = self.parse_path();
            self.skip_ws();
            if let Some(name) = maybe_name {
                if self.eat('=') {
                    let ty = self.parse()?;
                    assoc.push((name, ty));
                    self.skip_ws();
                    if self.eat(',') {
                        continue;
                    }
                    if self.eat('>') {
                        break;
                    }
                    return None;
                }
            }
            self.pos = save;
            args.push(self.parse()?);
            self.skip_ws();
            if self.eat(',') {
                continue;
            }
            if self.eat('>') {
                break;
            }
            return None;
        }
        Some((args, assoc))
    }

    fn starts_with(&self, text: &str) -> bool {
        let slice: String = self.chars[self.pos..]
            .iter()
            .take(text.chars().count())
            .collect();
        slice == text
    }

    fn parse_word(&mut self) -> Option<String> {
        self.skip_ws();
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_alphanumeric() || c == '_') {
            self.pos += 1;
        }
        if start == self.pos {
            None
        } else {
            Some(self.chars[start..self.pos].iter().collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_complex_types() {
        for text in [
            "i32",
            "Vec<i32>",
            "Option<Result<i32, string>>",
            "&mut [Option<i32>]",
            "(i32, bool)",
            "fn(i32, i32) -> bool",
            "dyn Iterator<Item = i64>",
        ] {
            let ty = parse_type(text).unwrap();
            assert_eq!(ty.to_string(), text, "round trip failed for {text}");
        }
    }

    #[test]
    fn head_drops_arguments() {
        let ty = parse_type("Vec<Option<i32>>").unwrap();
        assert_eq!(ty.head(), "Vec");
    }
}
