use crate::lexer::{lex, Token, TokenKind};
use crate::{Diagnostic, Span};

#[derive(Debug, Clone, Default)]
pub struct Module {
    pub package: Option<String>,
    pub imports: Vec<Import>,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub struct Import {
    pub path: String,
    pub alias: Option<String>,
    pub names: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub vis: bool,
    pub kind: ItemKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ItemKind {
    Const {
        name: String,
        ty: TypeExpr,
        value: Expr,
    },
    Struct {
        name: String,
        generics: Vec<String>,
        fields: Vec<Field>,
    },
    Enum {
        name: String,
        generics: Vec<String>,
        open: bool,
        variants: Vec<Variant>,
    },
    Opaque {
        name: String,
        ty: TypeExpr,
    },
    TypeAlias {
        name: String,
        ty: TypeExpr,
    },
    Interface {
        name: String,
        methods: Vec<Method>,
    },
    Impl {
        generics: Vec<String>,
        ty: TypeExpr,
        iface: Option<String>,
        items: Vec<Method>,
    },
    Fn(Function),
    Macro {
        name: String,
        rules: Vec<String>,
    },
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub ty: TypeExpr,
}

#[derive(Debug, Clone)]
pub struct Variant {
    pub name: String,
    pub fields: Vec<Field>,
    pub tuple: Vec<TypeExpr>,
}

#[derive(Debug, Clone)]
pub struct Method {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub body: Option<Block>,
}

#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub generics: Vec<String>,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub body: Option<Block>,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Option<TypeExpr>,
}

#[derive(Debug, Clone, Default)]
pub struct Block {
    pub stmts: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let {
        name: String,
        mutable: bool,
        ty: Option<TypeExpr>,
        value: Expr,
    },
    Assign {
        name: String,
        value: Expr,
    },
    Expr(Expr),
    Return(Option<Expr>),
    Break,
    Continue,
    Defer(Expr),
    If {
        cond: Expr,
        then: Block,
        else_: Option<Block>,
    },
    While {
        cond: Expr,
        body: Block,
    },
    For {
        name: String,
        iter: Expr,
        body: Block,
    },
    Match {
        value: Expr,
        arms: Vec<(Pattern, Option<Expr>, Expr)>,
    },
    Block(Block),
}

#[derive(Debug, Clone)]
pub enum Expr {
    Int(String),
    Float(String),
    String(String),
    Char(char),
    Bool(bool),
    Ident(String),
    Path(Vec<String>),
    SelfVal,
    Binary(Box<Expr>, String, Box<Expr>),
    Unary(String, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    Member(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    Try(Box<Expr>),
    Block(Block),
    Match(Box<Expr>, Vec<(Pattern, Option<Expr>, Expr)>),
    List(Vec<Expr>),
    Tuple(Vec<Expr>),
    Closure(Vec<Param>, Box<Expr>),
}

#[derive(Debug, Clone)]
pub enum Pattern {
    Wildcard,
    Ident(String),
    Literal(String),
    Path(Vec<String>, Vec<Pattern>),
}

#[derive(Debug, Clone)]
pub enum TypeExpr {
    Named(String),
    Path(Vec<String>),
    Ref(Box<TypeExpr>, bool),
    List(Box<TypeExpr>),
    Tuple(Vec<TypeExpr>),
    Generic(String, Vec<TypeExpr>),
    Dyn(String),
}

pub fn parse(source: &str) -> Result<Module, Vec<Diagnostic>> {
    let tokens = lex(source).map_err(|e| vec![e])?;
    Parser::new(tokens).parse_module()
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }
    fn peek(&self) -> &TokenKind {
        &self.tokens[self.pos].kind
    }
    fn current_span(&self) -> Span {
        self.tokens[self.pos].span.clone()
    }
    fn bump(&mut self) -> TokenKind {
        let kind = self.tokens[self.pos].kind.clone();
        if !matches!(kind, TokenKind::Eof) {
            self.pos += 1;
        }
        kind
    }
    fn error(&self, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            span: self.current_span(),
            message: message.into(),
        }
    }
    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.peek() == kind {
            self.bump();
            true
        } else {
            false
        }
    }
    fn ident(&mut self) -> Result<String, Diagnostic> {
        match self.bump() {
            TokenKind::Ident(s) => Ok(s),
            _ => Err(self.error("expected identifier")),
        }
    }
    fn skip_semi(&mut self) {
        while self.eat(&TokenKind::Semi) {}
    }

    fn parse_module(&mut self) -> Result<Module, Vec<Diagnostic>> {
        let mut module = Module::default();
        self.skip_semi();
        if self.eat(&TokenKind::Package) {
            let mut parts = vec![self.ident().map_err(|e| vec![e])?];
            while self.eat(&TokenKind::Dot) {
                parts.push(self.ident().map_err(|e| vec![e])?);
            }
            module.package = Some(parts.join("."));
            self.skip_semi();
        }
        while matches!(self.peek(), TokenKind::Import) {
            self.bump();
            let mut parts = vec![self.ident().map_err(|e| vec![e])?];
            while self.eat(&TokenKind::Dot) {
                if matches!(self.peek(), TokenKind::LBrace) {
                    break;
                }
                parts.push(self.ident().map_err(|e| vec![e])?);
            }
            let mut names = Vec::new();
            if self.eat(&TokenKind::LBrace) {
                loop {
                    names.push(self.ident().map_err(|e| vec![e])?);
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                    self.skip_semi();
                }
                if !self.eat(&TokenKind::RBrace) {
                    return Err(vec![self.error("expected }")]);
                }
            }
            let alias = if self.eat(&TokenKind::As) {
                Some(self.ident().map_err(|e| vec![e])?)
            } else {
                None
            };
            module.imports.push(Import {
                path: parts.join("."),
                alias,
                names,
            });
            self.skip_semi();
        }
        while !matches!(self.peek(), TokenKind::Eof) {
            self.skip_semi();
            if matches!(self.peek(), TokenKind::Eof) {
                break;
            }
            match self.parse_item() {
                Ok(item) => module.items.push(item),
                Err(e) => return Err(vec![e]),
            }
            self.skip_semi();
        }
        Ok(module)
    }

    fn parse_item(&mut self) -> Result<Item, Diagnostic> {
        let start = self.tokens[self.pos].span.start;
        let vis = self.eat(&TokenKind::Pub);
        let kind = match self.bump() {
            TokenKind::Const => {
                let name = self.ident()?;
                if !self.eat(&TokenKind::Colon) {
                    return Err(self.error("expected : after const name"));
                }
                let ty = self.parse_type()?;
                if !self.eat(&TokenKind::Eq) {
                    return Err(self.error("expected ="));
                }
                ItemKind::Const {
                    name,
                    ty,
                    value: self.parse_expr()?,
                }
            }
            TokenKind::Struct => {
                let name = self.ident()?;
                let generics = self.parse_generics();
                let fields = self.parse_fields()?;
                ItemKind::Struct {
                    name,
                    generics,
                    fields,
                }
            }
            TokenKind::Enum => {
                let name = self.ident()?;
                let generics = self.parse_generics();
                let variants = self.parse_variants()?;
                ItemKind::Enum {
                    name,
                    generics,
                    open: false,
                    variants,
                }
            }
            TokenKind::Open => {
                if !self.eat(&TokenKind::Enum) {
                    return Err(self.error("expected enum"));
                }
                let name = self.ident()?;
                let generics = self.parse_generics();
                let variants = self.parse_variants()?;
                ItemKind::Enum {
                    name,
                    generics,
                    open: true,
                    variants,
                }
            }
            TokenKind::Opaque => {
                let name = self.ident()?;
                if !self.eat(&TokenKind::Eq) {
                    return Err(self.error("expected ="));
                }
                ItemKind::Opaque {
                    name,
                    ty: self.parse_type()?,
                }
            }
            TokenKind::Type => {
                let name = self.ident()?;
                if !self.eat(&TokenKind::Eq) {
                    return Err(self.error("expected ="));
                }
                ItemKind::TypeAlias {
                    name,
                    ty: self.parse_type()?,
                }
            }
            TokenKind::Fn => ItemKind::Fn(self.parse_function()?),
            TokenKind::Interface => {
                let name = self.ident()?;
                if self.eat(&TokenKind::Colon) {
                    let _ = self.parse_type();
                }
                if !self.eat(&TokenKind::LBrace) {
                    return Err(self.error("expected {"));
                }
                let mut methods = Vec::new();
                while !self.eat(&TokenKind::RBrace) {
                    self.skip_semi();
                    if matches!(self.peek(), TokenKind::RBrace) {
                        break;
                    }
                    if self.eat(&TokenKind::Type) {
                        let _ = self.ident()?;
                        self.skip_semi();
                        continue;
                    }
                    if self.eat(&TokenKind::Fn) {
                        methods.push(self.parse_method()?);
                    } else {
                        return Err(self.error("expected interface item"));
                    }
                }
                ItemKind::Interface { name, methods }
            }
            TokenKind::Impl => {
                let generics = self.parse_generics();
                let ty = self.parse_type()?;
                let iface = if self.eat(&TokenKind::For) {
                    Some(self.parse_type()?.to_name())
                } else {
                    None
                };
                if !self.eat(&TokenKind::LBrace) {
                    return Err(self.error("expected {"));
                }
                let mut items = Vec::new();
                while !self.eat(&TokenKind::RBrace) {
                    self.skip_semi();
                    if matches!(self.peek(), TokenKind::RBrace) {
                        break;
                    }
                    if self.eat(&TokenKind::Fn) {
                        items.push(self.parse_method()?);
                    } else {
                        return Err(self.error("expected impl item"));
                    }
                }
                ItemKind::Impl {
                    generics,
                    ty,
                    iface,
                    items,
                }
            }
            TokenKind::Macro => {
                let name = self.ident()?;
                if self.eat(&TokenKind::Bang) {}
                if !self.eat(&TokenKind::LBrace) {
                    return Err(self.error("expected {"));
                }
                let start = self.pos;
                while !matches!(self.peek(), TokenKind::RBrace)
                    && !matches!(self.peek(), TokenKind::Eof)
                {
                    self.bump();
                }
                let end = self.pos;
                let rules = (start..end)
                    .map(|i| format!("{:?}", self.tokens[i].kind))
                    .collect();
                self.eat(&TokenKind::RBrace);
                ItemKind::Macro { name, rules }
            }
            _ => {
                return Err(Diagnostic {
                    span: self.current_span(),
                    message: "unsupported or unknown item".into(),
                })
            }
        };
        let end = self.tokens[self.pos.saturating_sub(1)].span.end;
        Ok(Item {
            vis,
            kind,
            span: Span::new(start, end),
        })
    }

    fn parse_generics(&mut self) -> Vec<String> {
        if !self.eat(&TokenKind::Lt) {
            return Vec::new();
        }
        let mut result = Vec::new();
        loop {
            if let TokenKind::Ident(name) = self.bump() {
                result.push(name);
            }
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        let _ = self.eat(&TokenKind::Gt);
        result
    }

    fn parse_fields(&mut self) -> Result<Vec<Field>, Diagnostic> {
        if !self.eat(&TokenKind::LBrace) {
            return Err(self.error("expected {"));
        }
        let mut fields = Vec::new();
        while !self.eat(&TokenKind::RBrace) {
            self.skip_semi();
            if matches!(self.peek(), TokenKind::RBrace) {
                break;
            }
            let name = self.ident()?;
            if !self.eat(&TokenKind::Colon) {
                return Err(self.error("expected :"));
            }
            let ty = self.parse_type()?;
            fields.push(Field { name, ty });
            if !self.eat(&TokenKind::Comma) {
                self.skip_semi();
            }
        }
        Ok(fields)
    }

    fn parse_variants(&mut self) -> Result<Vec<Variant>, Diagnostic> {
        if !self.eat(&TokenKind::LBrace) {
            return Err(self.error("expected {"));
        }
        let mut variants = Vec::new();
        while !self.eat(&TokenKind::RBrace) {
            self.skip_semi();
            if matches!(self.peek(), TokenKind::RBrace) {
                break;
            }
            let name = self.ident()?;
            let mut fields = Vec::new();
            let mut tuple = Vec::new();
            if self.eat(&TokenKind::LBrace) {
                while !self.eat(&TokenKind::RBrace) {
                    self.skip_semi();
                    if matches!(self.peek(), TokenKind::RBrace) {
                        break;
                    }
                    let field = self.ident()?;
                    if !self.eat(&TokenKind::Colon) {
                        return Err(self.error("expected :"));
                    }
                    fields.push(Field {
                        name: field,
                        ty: self.parse_type()?,
                    });
                    if !self.eat(&TokenKind::Comma) {
                        self.skip_semi();
                    }
                }
            } else if self.eat(&TokenKind::LParen) {
                while !self.eat(&TokenKind::RParen) {
                    tuple.push(self.parse_type()?);
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
            }
            variants.push(Variant {
                name,
                fields,
                tuple,
            });
            if !self.eat(&TokenKind::Comma) {
                self.skip_semi();
            }
        }
        Ok(variants)
    }

    fn parse_function(&mut self) -> Result<Function, Diagnostic> {
        let name = self.ident()?;
        let generics = self.parse_generics();
        if !self.eat(&TokenKind::LParen) {
            return Err(self.error("expected ("));
        }
        let params = self.parse_params()?;
        if !self.eat(&TokenKind::RParen) {
            return Err(self.error("expected )"));
        }
        let ret = if self.eat(&TokenKind::Arrow) {
            Some(self.parse_type()?)
        } else {
            None
        };
        let body = if self.eat(&TokenKind::LBrace) {
            Some(self.parse_block_after_lbrace()?)
        } else {
            None
        };
        Ok(Function {
            name,
            generics,
            params,
            ret,
            body,
        })
    }

    fn parse_method(&mut self) -> Result<Method, Diagnostic> {
        let name = self.ident()?;
        if !self.eat(&TokenKind::LParen) {
            return Err(self.error("expected ("));
        }
        let params = self.parse_params()?;
        if !self.eat(&TokenKind::RParen) {
            return Err(self.error("expected )"));
        }
        let ret = if self.eat(&TokenKind::Arrow) {
            Some(self.parse_type()?)
        } else {
            None
        };
        let body = if self.eat(&TokenKind::LBrace) {
            Some(self.parse_block_after_lbrace()?)
        } else {
            None
        };
        Ok(Method {
            name,
            params,
            ret,
            body,
        })
    }

    fn parse_params(&mut self) -> Result<Vec<Param>, Diagnostic> {
        let mut params = Vec::new();
        while !matches!(self.peek(), TokenKind::RParen) {
            self.skip_semi();
            let name = self.ident()?;
            let ty = if self.eat(&TokenKind::Colon) {
                Some(self.parse_type()?)
            } else {
                None
            };
            params.push(Param { name, ty });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        Ok(params)
    }

    fn parse_type(&mut self) -> Result<TypeExpr, Diagnostic> {
        let ty = match self.bump() {
            TokenKind::Amp => TypeExpr::Ref(Box::new(self.parse_type()?), false),
            TokenKind::LBracket => {
                let inner = self.parse_type()?;
                if !self.eat(&TokenKind::RBracket) {
                    return Err(self.error("expected ]"));
                }
                TypeExpr::List(Box::new(inner))
            }
            TokenKind::Dyn => {
                if let TokenKind::Ident(name) = self.bump() {
                    TypeExpr::Dyn(name)
                } else {
                    return Err(self.error("expected interface name"));
                }
            }
            TokenKind::Ident(name) => {
                let mut parts = vec![name];
                while self.eat(&TokenKind::ColonColon) {
                    if let TokenKind::Ident(part) = self.bump() {
                        parts.push(part);
                    } else {
                        return Err(self.error("expected path segment"));
                    }
                }
                if self.eat(&TokenKind::Lt) {
                    let mut args = Vec::new();
                    loop {
                        args.push(self.parse_type()?);
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    if !self.eat(&TokenKind::Gt) {
                        return Err(self.error("expected >"));
                    }
                    TypeExpr::Generic(parts.join("::"), args)
                } else if parts.len() == 1 {
                    TypeExpr::Named(parts.remove(0))
                } else {
                    TypeExpr::Path(parts)
                }
            }
            TokenKind::LParen => {
                let mut types = Vec::new();
                while !self.eat(&TokenKind::RParen) {
                    types.push(self.parse_type()?);
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                TypeExpr::Tuple(types)
            }
            _ => {
                return Err(Diagnostic {
                    span: self.current_span(),
                    message: "expected type".into(),
                })
            }
        };
        Ok(ty)
    }

    fn parse_block_after_lbrace(&mut self) -> Result<Block, Diagnostic> {
        let mut stmts = Vec::new();
        while !self.eat(&TokenKind::RBrace) {
            self.skip_semi();
            if matches!(self.peek(), TokenKind::RBrace) {
                break;
            }
            stmts.push(self.parse_stmt()?);
            self.skip_semi();
        }
        Ok(Block { stmts })
    }

    fn parse_stmt(&mut self) -> Result<Stmt, Diagnostic> {
        match self.peek().clone() {
            TokenKind::Let => {
                self.bump();
                let mutable = self.eat(&TokenKind::Mut);
                let name = self.ident()?;
                let ty = if self.eat(&TokenKind::Colon) {
                    Some(self.parse_type()?)
                } else {
                    None
                };
                if !self.eat(&TokenKind::Eq) {
                    return Err(self.error("expected ="));
                }
                Ok(Stmt::Let {
                    name,
                    mutable,
                    ty,
                    value: self.parse_expr()?,
                })
            }
            TokenKind::Return => {
                self.bump();
                Ok(Stmt::Return(
                    if matches!(self.peek(), TokenKind::Semi | TokenKind::RBrace) {
                        None
                    } else {
                        Some(self.parse_expr()?)
                    },
                ))
            }
            TokenKind::Break => {
                self.bump();
                Ok(Stmt::Break)
            }
            TokenKind::Continue => {
                self.bump();
                Ok(Stmt::Continue)
            }
            TokenKind::Defer => {
                self.bump();
                Ok(Stmt::Defer(self.parse_expr()?))
            }
            TokenKind::If => {
                self.bump();
                let cond = self.parse_expr()?;
                if !self.eat(&TokenKind::LBrace) {
                    return Err(self.error("expected {"));
                }
                let then = self.parse_block_after_lbrace()?;
                let else_ = if self.eat(&TokenKind::Else) {
                    if self.eat(&TokenKind::If) {
                        None
                    } else {
                        if self.eat(&TokenKind::LBrace) {
                            Some(self.parse_block_after_lbrace()?)
                        } else {
                            None
                        }
                    }
                } else {
                    None
                };
                Ok(Stmt::If { cond, then, else_ })
            }
            TokenKind::While => {
                self.bump();
                let cond = self.parse_expr()?;
                if !self.eat(&TokenKind::LBrace) {
                    return Err(self.error("expected {"));
                }
                Ok(Stmt::While {
                    cond,
                    body: self.parse_block_after_lbrace()?,
                })
            }
            TokenKind::For => {
                self.bump();
                let name = self.ident()?;
                if !self.eat(&TokenKind::In) {
                    return Err(self.error("expected in"));
                }
                let iter = self.parse_expr()?;
                if !self.eat(&TokenKind::LBrace) {
                    return Err(self.error("expected {"));
                }
                Ok(Stmt::For {
                    name,
                    iter,
                    body: self.parse_block_after_lbrace()?,
                })
            }
            TokenKind::Match => {
                self.bump();
                let value = self.parse_expr()?;
                if !self.eat(&TokenKind::LBrace) {
                    return Err(self.error("expected {"));
                }
                let arms = self.parse_arms()?;
                Ok(Stmt::Match { value, arms })
            }
            TokenKind::LBrace => {
                self.bump();
                Ok(Stmt::Block(self.parse_block_after_lbrace()?))
            }
            _ => {
                let expr = self.parse_expr()?;
                if self.eat(&TokenKind::Eq) {
                    if let Expr::Ident(name) = expr {
                        let value = self.parse_expr()?;
                        Ok(Stmt::Assign { name, value })
                    } else {
                        Err(self.error("invalid assignment target"))
                    }
                } else {
                    Ok(Stmt::Expr(expr))
                }
            }
        }
    }

    fn parse_arms(&mut self) -> Result<Vec<(Pattern, Option<Expr>, Expr)>, Diagnostic> {
        let mut arms = Vec::new();
        while !self.eat(&TokenKind::RBrace) {
            self.skip_semi();
            if matches!(self.peek(), TokenKind::RBrace) {
                break;
            }
            let pattern = self.parse_pattern()?;
            let guard = if self.eat(&TokenKind::If) {
                Some(self.parse_expr()?)
            } else {
                None
            };
            if !self.eat(&TokenKind::FatArrow) {
                return Err(self.error("expected =>"));
            }
            let expr = self.parse_expr()?;
            arms.push((pattern, guard, expr));
            self.skip_semi();
            if !matches!(self.peek(), TokenKind::Comma) && !matches!(self.peek(), TokenKind::RBrace)
            {
                break;
            }
            self.eat(&TokenKind::Comma);
        }
        Ok(arms)
    }

    fn parse_pattern(&mut self) -> Result<Pattern, Diagnostic> {
        match self.bump() {
            TokenKind::Ident(name) if name == "_" => Ok(Pattern::Wildcard),
            TokenKind::Ident(name) => {
                if self.eat(&TokenKind::ColonColon) {
                    let mut parts = vec![name];
                    if let TokenKind::Ident(part) = self.bump() {
                        parts.push(part);
                    }
                    let pats = if self.eat(&TokenKind::LBrace) {
                        self.parse_field_patterns()?
                    } else {
                        Vec::new()
                    };
                    Ok(Pattern::Path(parts, pats))
                } else {
                    Ok(Pattern::Ident(name))
                }
            }
            TokenKind::Integer(s) => Ok(Pattern::Literal(s)),
            TokenKind::Float(s) => Ok(Pattern::Literal(s)),
            TokenKind::String(s) => Ok(Pattern::Literal(format!("{:?}", s))),
            TokenKind::True => Ok(Pattern::Literal("true".into())),
            TokenKind::False => Ok(Pattern::Literal("false".into())),
            _ => Err(self.error("unsupported pattern")),
        }
    }

    fn parse_field_patterns(&mut self) -> Result<Vec<Pattern>, Diagnostic> {
        let mut pats = Vec::new();
        while !self.eat(&TokenKind::RBrace) {
            self.skip_semi();
            if matches!(self.peek(), TokenKind::RBrace) {
                break;
            }
            if let TokenKind::Ident(_) = self.peek() {
                pats.push(self.parse_pattern()?);
            } else {
                let _ = self.bump();
            }
            self.eat(&TokenKind::Comma);
        }
        Ok(pats)
    }

    fn parse_expr(&mut self) -> Result<Expr, Diagnostic> {
        let mut left = self.parse_unary()?;
        while matches!(
            self.peek(),
            TokenKind::Plus
                | TokenKind::Minus
                | TokenKind::Star
                | TokenKind::Slash
                | TokenKind::Percent
                | TokenKind::EqEq
                | TokenKind::BangEq
                | TokenKind::Lt
                | TokenKind::LtEq
                | TokenKind::Gt
                | TokenKind::GtEq
        ) {
            let op = format!("{:?}", self.bump());
            left = Expr::Binary(Box::new(left), op, Box::new(self.parse_unary()?));
        }
        while matches!(self.peek(), TokenKind::LParen) {
            self.bump();
            let mut args = Vec::new();
            while !matches!(self.peek(), TokenKind::RParen) {
                args.push(self.parse_expr()?);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            if !self.eat(&TokenKind::RParen) {
                return Err(self.error("expected )"));
            }
            left = Expr::Call(Box::new(left), args);
        }
        if self.eat(&TokenKind::Question) {
            left = Expr::Try(Box::new(left));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, Diagnostic> {
        match self.peek().clone() {
            TokenKind::Minus | TokenKind::Bang | TokenKind::Amp => {
                let op = format!("{:?}", self.bump());
                Ok(Expr::Unary(op, Box::new(self.parse_unary()?)))
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, Diagnostic> {
        let expr = match self.bump() {
            TokenKind::Integer(s) => Expr::Int(s),
            TokenKind::Float(s) => Expr::Float(s),
            TokenKind::String(s) => Expr::String(s),
            TokenKind::Char(c) => Expr::Char(c),
            TokenKind::True => Expr::Bool(true),
            TokenKind::False => Expr::Bool(false),
            TokenKind::Ident(name) => {
                if self.eat(&TokenKind::Bang) {
                    if self.eat(&TokenKind::LBrace) {
                        let _ = self.parse_block_after_lbrace()?;
                    }
                    Expr::Ident(format!("{}!", name))
                } else if self.eat(&TokenKind::ColonColon) {
                    let mut parts = vec![name];
                    if let TokenKind::Ident(part) = self.bump() {
                        parts.push(part);
                    }
                    Expr::Path(parts)
                } else {
                    Expr::Ident(name)
                }
            }
            TokenKind::SelfVal => Expr::SelfVal,
            TokenKind::LParen => {
                let mut values = Vec::new();
                while !self.eat(&TokenKind::RParen) {
                    values.push(self.parse_expr()?);
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                if values.len() == 1 {
                    values.remove(0)
                } else {
                    Expr::Tuple(values)
                }
            }
            TokenKind::LBracket => {
                let mut values = Vec::new();
                while !self.eat(&TokenKind::RBracket) {
                    values.push(self.parse_expr()?);
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                Expr::List(values)
            }
            TokenKind::LBrace => Expr::Block(self.parse_block_after_lbrace()?),
            TokenKind::Match => {
                let value = self.parse_expr()?;
                if !self.eat(&TokenKind::LBrace) {
                    return Err(self.error("expected {"));
                }
                Expr::Match(Box::new(value), self.parse_arms()?)
            }
            TokenKind::Try => {
                if !self.eat(&TokenKind::LBrace) {
                    return Err(self.error("expected {"));
                }
                Expr::Try(Box::new(Expr::Block(self.parse_block_after_lbrace()?)))
            }
            _ => {
                return Err(Diagnostic {
                    span: self.current_span(),
                    message: "expected expression".into(),
                })
            }
        };
        let mut expr = expr;
        while matches!(self.peek(), TokenKind::Dot) {
            self.bump();
            if let TokenKind::Ident(name) = self.bump() {
                expr = Expr::Member(Box::new(expr), name);
            } else {
                return Err(self.error("expected member name"));
            }
        }
        Ok(expr)
    }
}

impl TypeExpr {
    pub fn to_name(&self) -> String {
        match self {
            TypeExpr::Named(s) => s.clone(),
            TypeExpr::Path(p) => p.join("::"),
            TypeExpr::Generic(n, _) => n.clone(),
            TypeExpr::Dyn(n) => n.clone(),
            _ => "()".into(),
        }
    }
}
