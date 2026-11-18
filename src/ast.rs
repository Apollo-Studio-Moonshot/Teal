//! Surface AST. The parser writes this tree directly; the checker consumes it
//! per function body. There is deliberately only one IR (DESIGN.md §4.1).

use crate::macros::TokenTree;
use crate::Span;
use std::fmt;

#[derive(Clone, Debug, Default)]
pub struct Module {
    pub package: Option<String>,
    pub package_span: Option<Span>,
    pub docs: Vec<String>,
    pub imports: Vec<Import>,
    pub items: Vec<Item>,
}

impl Module {
    pub fn package_name(&self, fallback: &str) -> String {
        self.package.clone().unwrap_or_else(|| fallback.to_string())
    }
}

#[derive(Clone, Debug)]
pub struct Import {
    pub path: Vec<String>,
    pub alias: Option<String>,
    pub names: Vec<String>,
    pub span: Span,
}

impl Import {
    /// The package path this import refers to. For `import a.b.{C}` this is
    /// `a.b`; for `import a.b` it is also `a.b`.
    pub fn package(&self) -> String {
        self.path.join(".")
    }

    /// The name a namespace import binds: the alias if present, otherwise the
    /// last path segment.
    pub fn binding(&self) -> Option<String> {
        if self.alias.is_some() {
            self.alias.clone()
        } else if self.names.is_empty() {
            self.path.last().cloned()
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Private,
    Public,
}

#[derive(Clone, Debug)]
pub struct Attribute {
    pub name: String,
    pub args: Vec<String>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub attrs: Vec<Attribute>,
    pub vis: Visibility,
    pub kind: ItemKind,
    pub span: Span,
    pub docs: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum ItemKind {
    Const {
        name: String,
        ty: TypeExpr,
        value: Expr,
    },
    Struct {
        name: String,
        generics: Generics,
        fields: Vec<Field>,
    },
    Enum {
        name: String,
        generics: Generics,
        open: bool,
        variants: Vec<Variant>,
    },
    Opaque {
        name: String,
        ty: TypeExpr,
    },
    TypeAlias {
        name: String,
        generics: Generics,
        ty: TypeExpr,
    },
    Interface {
        name: String,
        generics: Generics,
        assoc_types: Vec<AssocType>,
        methods: Vec<Method>,
    },
    Impl {
        generics: Generics,
        iface: Option<TypeExpr>,
        ty: TypeExpr,
        assoc: Vec<AssocBinding>,
        methods: Vec<Method>,
    },
    Fn(Function),
    Macro(crate::macros::MacroDef),
}

impl ItemKind {
    pub fn name(&self) -> Option<&str> {
        Some(match self {
            ItemKind::Const { name, .. }
            | ItemKind::Struct { name, .. }
            | ItemKind::Enum { name, .. }
            | ItemKind::Opaque { name, .. }
            | ItemKind::TypeAlias { name, .. }
            | ItemKind::Interface { name, .. } => name,
            ItemKind::Fn(function) => &function.name,
            ItemKind::Macro(def) => &def.name,
            ItemKind::Impl { .. } => return None,
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct Generics {
    pub params: Vec<GenericParam>,
}

impl Generics {
    pub fn names(&self) -> Vec<String> {
        self.params.iter().map(|p| p.name.clone()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.params.is_empty()
    }

    pub fn print(&self) -> String {
        if self.params.is_empty() {
            return String::new();
        }
        format!(
            "<{}>",
            self.params
                .iter()
                .map(|p| p.print())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

#[derive(Clone, Debug)]
pub struct GenericParam {
    pub name: String,
    pub bounds: Vec<Bound>,
    pub span: Span,
}

impl GenericParam {
    pub fn print(&self) -> String {
        if self.bounds.is_empty() {
            self.name.clone()
        } else {
            format!(
                "{}: {}",
                self.name,
                self.bounds
                    .iter()
                    .map(|b| b.print())
                    .collect::<Vec<_>>()
                    .join(" + ")
            )
        }
    }
}

#[derive(Clone, Debug)]
pub struct Bound {
    pub path: Vec<String>,
    pub args: Vec<TypeExpr>,
    pub assoc: Vec<(String, TypeExpr)>,
    pub fn_sig: Option<(Vec<TypeExpr>, Box<TypeExpr>)>,
    pub span: Span,
}

impl Bound {
    pub fn print(&self) -> String {
        let name = self.path.join("::");
        let mut out = if let Some((params, ret)) = &self.fn_sig {
            format!(
                "{} -> {}",
                params
                    .iter()
                    .map(TypeExpr::print)
                    .collect::<Vec<_>>()
                    .join(", "),
                ret.print()
            )
        } else if self.args.is_empty() && self.assoc.is_empty() {
            name
        } else {
            let mut parts: Vec<String> = self.args.iter().map(TypeExpr::print).collect();
            parts.extend(
                self.assoc
                    .iter()
                    .map(|(k, v)| format!("{} = {}", k, v.print())),
            );
            format!("{}<{}>", name, parts.join(", "))
        };
        if out.starts_with("Fn") {
            out = format!("Fn({})", out.trim_start_matches("Fn "));
        }
        out
    }
}

#[derive(Clone, Debug)]
pub struct Field {
    pub name: String,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Variant {
    pub name: String,
    pub fields: Vec<Field>,
    pub tuple: Vec<TypeExpr>,
    pub span: Span,
}

impl Variant {
    pub fn is_unit(&self) -> bool {
        self.fields.is_empty() && self.tuple.is_empty()
    }

    pub fn arity(&self) -> usize {
        if self.tuple.is_empty() {
            self.fields.len()
        } else {
            self.tuple.len()
        }
    }
}

#[derive(Clone, Debug)]
pub struct AssocType {
    pub name: String,
    pub bounds: Vec<Bound>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct AssocBinding {
    pub name: String,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Function {
    pub name: String,
    pub generics: Generics,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub body: Option<Block>,
    pub span: Span,
    pub docs: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Method {
    pub name: String,
    pub generics: Generics,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub body: Option<Block>,
    pub span: Span,
    pub docs: Vec<String>,
}

impl Method {
    pub fn signature_text(&self) -> String {
        format!(
            "fn {}{}({}){}{}",
            self.name,
            self.generics.print(),
            self.params
                .iter()
                .map(Param::print)
                .collect::<Vec<_>>()
                .join(", "),
            self.ret
                .as_ref()
                .map(|t| format!(" -> {}", t.print()))
                .unwrap_or_default(),
            if self.body.is_some() { " { ... }" } else { "" }
        )
    }
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub ty: Option<TypeExpr>,
    pub mutable: bool,
    pub span: Span,
}

impl Param {
    pub fn print(&self) -> String {
        let prefix = if self.mutable { "mut " } else { "" };
        match &self.ty {
            Some(ty) => format!("{}{}: {}", prefix, self.name, ty.print()),
            None => format!("{}{}", prefix, self.name),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

impl Block {
    pub fn empty(span: Span) -> Self {
        Self {
            stmts: Vec::new(),
            span,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
    /// True when the statement was terminated by `;` (explicit or inserted).
    /// A final expression without a terminator is the block's tail expression.
    pub semi: bool,
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    Let {
        pattern: Pattern,
        mutable: bool,
        ty: Option<TypeExpr>,
        value: Expr,
        else_block: Option<Block>,
    },
    Assign {
        target: Expr,
        op: Option<BinOp>,
        value: Expr,
    },
    Defer(Expr),
    Expr(Expr),
    Return(Option<Expr>),
    Break,
    Continue,
    Item(Box<Item>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Shl,
    Shr,
    BitAnd,
    BitOr,
    BitXor,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    AndAnd,
    OrOr,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::AndAnd => "&&",
            BinOp::OrOr => "||",
        }
    }

    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        )
    }

    pub fn is_logical(self) -> bool {
        matches!(self, BinOp::AndAnd | BinOp::OrOr)
    }

    pub fn is_arithmetic(self) -> bool {
        matches!(
            self,
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem
        )
    }

    pub fn is_bitwise(self) -> bool {
        matches!(
            self,
            BinOp::Shl | BinOp::Shr | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor
        )
    }

    pub fn precedence(self) -> u8 {
        match self {
            BinOp::OrOr => 1,
            BinOp::AndAnd => 2,
            BinOp::BitOr => 3,
            BinOp::BitXor => 4,
            BinOp::BitAnd => 5,
            BinOp::Eq | BinOp::Ne => 6,
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 7,
            BinOp::Shl | BinOp::Shr => 8,
            BinOp::Add | BinOp::Sub => 9,
            BinOp::Mul | BinOp::Div | BinOp::Rem => 10,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    Ref,
    RefMut,
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Self { kind, span }
    }
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Int {
        text: String,
        value: i128,
        suffix: Option<String>,
    },
    Float {
        text: String,
        value: f64,
        suffix: Option<String>,
    },
    Str(String),
    Char(char),
    Bool(bool),
    Path {
        path: Vec<String>,
        args: Vec<TypeExpr>,
    },
    SelfValue,
    Unary {
        op: UnOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Range {
        lo: Option<Box<Expr>>,
        hi: Option<Box<Expr>>,
        inclusive: bool,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    MethodCall {
        receiver: Box<Expr>,
        name: String,
        args: Vec<Expr>,
        span: Span,
    },
    Field {
        base: Box<Expr>,
        name: String,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    Try(Box<Expr>),
    Cast {
        expr: Box<Expr>,
        ty: TypeExpr,
    },
    Block(Block),
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    If {
        cond: Box<Expr>,
        then_branch: Block,
        else_branch: Option<Box<Expr>>,
    },
    IfLet {
        pattern: Pattern,
        scrutinee: Box<Expr>,
        then_branch: Block,
        else_branch: Option<Box<Expr>>,
    },
    While {
        cond: Box<Expr>,
        body: Block,
    },
    WhileLet {
        pattern: Pattern,
        scrutinee: Box<Expr>,
        body: Block,
    },
    Loop {
        body: Block,
    },
    For {
        pattern: Pattern,
        iter: Box<Expr>,
        body: Block,
    },
    Closure {
        params: Vec<Param>,
        ret: Option<TypeExpr>,
        body: Box<Expr>,
    },
    List(Vec<Expr>),
    Tuple(Vec<Expr>),
    StructLit {
        path: Vec<String>,
        args: Vec<TypeExpr>,
        fields: Vec<(String, Expr)>,
        span: Span,
    },
    MacroCall {
        name: String,
        tokens: Vec<TokenTree>,
        span: Span,
    },
}

#[derive(Clone, Debug)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Pattern {
    pub kind: PatKind,
    pub span: Span,
}

impl Pattern {
    pub fn new(kind: PatKind, span: Span) -> Self {
        Self { kind, span }
    }

    pub fn is_irrefutable(&self) -> bool {
        match &self.kind {
            PatKind::Wildcard | PatKind::Binding { .. } => true,
            PatKind::Ref { pattern, .. } => pattern.is_irrefutable(),
            _ => false,
        }
    }

    pub fn binding_names(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.collect_bindings(&mut out);
        out
    }

    fn collect_bindings(&self, out: &mut Vec<String>) {
        match &self.kind {
            PatKind::Binding { name, sub } => {
                out.push(name.clone());
                if let Some(sub) = sub {
                    sub.collect_bindings(out);
                }
            }
            PatKind::Tuple(items) | PatKind::Or(items) => {
                for item in items {
                    item.collect_bindings(out);
                }
            }
            PatKind::Path { args, .. } => {
                for arg in args {
                    arg.collect_bindings(out);
                }
            }
            PatKind::Struct { fields, .. } => {
                for (_, pat) in fields {
                    pat.collect_bindings(out);
                }
            }
            PatKind::Ref { pattern, .. } => pattern.collect_bindings(out),
            PatKind::Wildcard | PatKind::Literal(_) | PatKind::Range { .. } => {}
        }
    }
}

#[derive(Clone, Debug)]
pub enum PatKind {
    Wildcard,
    Binding {
        name: String,
        sub: Option<Box<Pattern>>,
    },
    Literal(PatLiteral),
    Path {
        path: Vec<String>,
        args: Vec<Pattern>,
    },
    Struct {
        path: Vec<String>,
        fields: Vec<(String, Pattern)>,
        rest: bool,
    },
    Tuple(Vec<Pattern>),
    Or(Vec<Pattern>),
    Range {
        lo: PatLiteral,
        hi: PatLiteral,
        inclusive: bool,
    },
    Ref {
        mutable: bool,
        pattern: Box<Pattern>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum PatLiteral {
    Int(String, i128),
    Float(String, f64),
    Str(String),
    Char(char),
    Bool(bool),
}

impl PatLiteral {
    pub fn print(&self) -> String {
        match self {
            PatLiteral::Int(text, _) => text.clone(),
            PatLiteral::Float(text, _) => text.clone(),
            PatLiteral::Str(value) => format!("{:?}", value),
            PatLiteral::Char(c) => format!("{:?}", c),
            PatLiteral::Bool(b) => b.to_string(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct TypeExpr {
    pub kind: TypeExprKind,
    pub span: Span,
}

impl TypeExpr {
    pub fn new(kind: TypeExprKind, span: Span) -> Self {
        Self { kind, span }
    }

    pub fn unit(span: Span) -> Self {
        Self::new(TypeExprKind::Unit, span)
    }

    pub fn named(name: &str, span: Span) -> Self {
        Self::new(
            TypeExprKind::Path {
                path: name.split("::").map(str::to_string).collect(),
                args: Vec::new(),
                assoc: Vec::new(),
            },
            span,
        )
    }

    pub fn print(&self) -> String {
        self.to_string()
    }

    /// Simple last-segment name, used for diagnostics and tli keys.
    pub fn name(&self) -> String {
        match &self.kind {
            TypeExprKind::Path { path, .. } => path.last().cloned().unwrap_or_default(),
            TypeExprKind::Dyn { path, .. } => path.last().cloned().unwrap_or_default(),
            TypeExprKind::Ref { inner, .. } => format!("&{}", inner.name()),
            TypeExprKind::Slice(inner) => format!("[{}]", inner.name()),
            TypeExprKind::Tuple(items) => format!(
                "({})",
                items
                    .iter()
                    .map(TypeExpr::name)
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            TypeExprKind::Fn { .. } => "Fn".into(),
            TypeExprKind::Unit => "()".into(),
        }
    }
}

impl fmt::Display for TypeExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            TypeExprKind::Path { path, args, assoc } => {
                write!(f, "{}", path.join("::"))?;
                if !args.is_empty() || !assoc.is_empty() {
                    let mut parts: Vec<String> = args.iter().map(|t| t.to_string()).collect();
                    parts.extend(assoc.iter().map(|(k, v)| format!("{} = {}", k, v)));
                    write!(f, "<{}>", parts.join(", "))?;
                }
                Ok(())
            }
            TypeExprKind::Ref { inner, mutable } => {
                if *mutable {
                    write!(f, "&mut {}", inner)
                } else {
                    write!(f, "&{}", inner)
                }
            }
            TypeExprKind::Slice(inner) => write!(f, "[{}]", inner),
            TypeExprKind::Tuple(items) => {
                write!(
                    f,
                    "({})",
                    items
                        .iter()
                        .map(|t| t.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
            TypeExprKind::Fn { params, ret } => write!(
                f,
                "Fn({}) -> {}",
                params
                    .iter()
                    .map(|t| t.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                ret
            ),
            TypeExprKind::Dyn { path, args, assoc } => {
                write!(f, "dyn {}", path.join("::"))?;
                if !args.is_empty() || !assoc.is_empty() {
                    let mut parts: Vec<String> = args.iter().map(|t| t.to_string()).collect();
                    parts.extend(assoc.iter().map(|(k, v)| format!("{} = {}", k, v)));
                    write!(f, "<{}>", parts.join(", "))?;
                }
                Ok(())
            }
            TypeExprKind::Unit => write!(f, "()"),
        }
    }
}

#[derive(Clone, Debug)]
pub enum TypeExprKind {
    Path {
        path: Vec<String>,
        args: Vec<TypeExpr>,
        assoc: Vec<(String, TypeExpr)>,
    },
    Ref {
        inner: Box<TypeExpr>,
        mutable: bool,
    },
    Slice(Box<TypeExpr>),
    Tuple(Vec<TypeExpr>),
    Fn {
        params: Vec<TypeExpr>,
        ret: Box<TypeExpr>,
    },
    Dyn {
        path: Vec<String>,
        args: Vec<TypeExpr>,
        assoc: Vec<(String, TypeExpr)>,
    },
    Unit,
}

/// Parse a decimal/hex/binary/octal integer literal and strip any type suffix.
pub fn parse_int_literal(text: &str) -> Option<(i128, Option<String>)> {
    let mut digits = String::new();
    let mut suffix = None;
    let mut chars = text.chars().peekable();
    let mut radix = 10;
    if text.starts_with("0x") || text.starts_with("0X") {
        radix = 16;
        chars.next();
        chars.next();
    } else if text.starts_with("0b") || text.starts_with("0B") {
        radix = 2;
        chars.next();
        chars.next();
    } else if text.starts_with("0o") || text.starts_with("0O") {
        radix = 8;
        chars.next();
        chars.next();
    }
    while let Some(&c) = chars.peek() {
        if c == '_' {
            chars.next();
        } else if c.is_digit(radix) {
            digits.push(c);
            chars.next();
        } else {
            break;
        }
    }
    let rest: String = chars.collect();
    if !rest.is_empty() {
        suffix = Some(rest);
    }
    i128::from_str_radix(&digits, radix)
        .ok()
        .map(|v| (v, suffix))
}

pub fn parse_float_literal(text: &str) -> Option<(f64, Option<String>)> {
    let mut numeric = String::new();
    let mut suffix = String::new();
    let mut in_suffix = false;
    for c in text.chars() {
        if in_suffix {
            suffix.push(c);
        } else if c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-') {
            numeric.push(c);
        } else if c == '_' {
            continue;
        } else {
            in_suffix = true;
            suffix.push(c);
        }
    }
    numeric.parse::<f64>().ok().map(|v| {
        (
            v,
            if suffix.is_empty() {
                None
            } else {
                Some(suffix)
            },
        )
    })
}
