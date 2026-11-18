//! LL(1) recursive-descent parser.
//!
//! Every item starts with a keyword, every statement starts with a keyword,
//! `{`, or a primary expression, and expression-position type arguments require
//! `::<`, so one token of lookahead (two for closures) resolves the grammar.
//! Macro invocations are expanded inline while parsing, as required by
//! DESIGN.md §4.1.

use crate::ast::*;
use crate::lexer::{lex, Token, TokenKind};
use crate::macros::{self, Delim, Expander, MacroDef, MacroRule, TokenTree};
use crate::{codes, Diagnostic, FileId, Span};
use std::collections::BTreeMap;

pub fn parse_module(
    source: &str,
    file: FileId,
    package: &str,
    prelude: &BTreeMap<String, MacroDef>,
) -> Result<Module, Vec<Diagnostic>> {
    let tokens = lex(source, file).map_err(|e| vec![e])?;
    let mut parser = Parser::new(tokens, file, prelude.clone(), package);
    match parser.parse_module() {
        Ok(module) => {
            if parser.errors.is_empty() {
                Ok(module)
            } else {
                Err(parser.errors)
            }
        }
        Err(err) => {
            parser.errors.push(err);
            Err(parser.errors)
        }
    }
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    file: FileId,
    package: String,
    macros: BTreeMap<String, MacroDef>,
    no_struct_lit: bool,
    expansion_depth: usize,
    errors: Vec<Diagnostic>,
}

impl Parser {
    fn new(
        tokens: Vec<Token>,
        file: FileId,
        macros: BTreeMap<String, MacroDef>,
        package: &str,
    ) -> Self {
        Self {
            tokens,
            pos: 0,
            file,
            package: package.to_string(),
            macros,
            no_struct_lit: false,
            expansion_depth: 0,
            errors: Vec::new(),
        }
    }

    fn peek(&self) -> &TokenKind {
        &self.tokens[self.pos].kind
    }

    fn peek_at(&self, offset: usize) -> &TokenKind {
        self.tokens
            .get(self.pos + offset)
            .map(|t| &t.kind)
            .unwrap_or(&TokenKind::Eof)
    }

    fn span(&self) -> Span {
        self.tokens[self.pos].span
    }

    fn prev_span(&self) -> Span {
        self.tokens[self.pos.saturating_sub(1)].span
    }

    fn bump(&mut self) -> TokenKind {
        let kind = self.tokens[self.pos].kind.clone();
        if !matches!(kind, TokenKind::Eof) {
            self.pos += 1;
        }
        kind
    }

    fn error(&self, message: impl Into<String>) -> Diagnostic {
        Diagnostic::error(codes::PARSE, self.span(), message)
    }

    fn error_at(&self, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic::error(codes::PARSE, span, message)
    }

    fn at(&self, kind: &TokenKind) -> bool {
        self.peek() == kind
    }

    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: &TokenKind, what: &str) -> Result<Span, Diagnostic> {
        if self.at(kind) {
            let span = self.span();
            self.bump();
            Ok(span)
        } else {
            Err(self.error(format!("expected {what}, found {}", self.peek())))
        }
    }

    fn ident(&mut self) -> Result<(String, Span), Diagnostic> {
        let span = self.span();
        match self.bump() {
            TokenKind::Ident(name) => Ok((name, span)),
            other => Err(self.error_at(span, format!("expected identifier, found {other}"))),
        }
    }

    fn skip_semis(&mut self) {
        while self.eat(&TokenKind::Semi) {}
    }

    /// Doc comments are only meaningful on items; inside field/variant/param
    /// lists they are skipped rather than treated as tokens.
    fn skip_docs(&mut self) {
        while matches!(self.peek(), TokenKind::DocComment(_)) {
            self.bump();
        }
    }

    /// Split a `>>` token into two `>` tokens, then consume one. This is the
    /// classic nested-generics lexing issue (`Vec<Vec<T>>`).
    fn eat_gt(&mut self) -> bool {
        match self.peek().clone() {
            TokenKind::Gt => {
                self.bump();
                true
            }
            TokenKind::Shr => {
                let span = self.span();
                self.tokens[self.pos].kind = TokenKind::Gt;
                self.tokens.insert(
                    self.pos + 1,
                    Token {
                        kind: TokenKind::Gt,
                        span,
                    },
                );
                self.bump();
                true
            }
            _ => false,
        }
    }

    fn expect_gt(&mut self, what: &str) -> Result<(), Diagnostic> {
        if self.eat_gt() {
            Ok(())
        } else {
            Err(self.error(format!(
                "expected `>` to close {what}, found {}",
                self.peek()
            )))
        }
    }

    // ---------------------------------------------------------------- module

    fn parse_module(&mut self) -> Result<Module, Diagnostic> {
        let mut module = Module::default();
        module.docs = self.collect_docs();
        self.skip_semis();

        if self.at(&TokenKind::Package) {
            let span = self.span();
            self.bump();
            let mut parts = vec![self.ident()?.0];
            while self.eat(&TokenKind::ColonColon) {
                parts.push(self.ident()?.0);
            }
            while self.eat(&TokenKind::Dot) {
                parts.push(self.ident()?.0);
            }
            module.package = Some(parts.join("."));
            module.package_span = Some(span.merge(self.prev_span()));
            self.package = parts.join(".");
            self.skip_semis();
        }

        while self.at(&TokenKind::Import) {
            let span = self.span();
            self.bump();
            let mut path = vec![self.ident()?.0];
            loop {
                if self.at(&TokenKind::ColonColon) || self.at(&TokenKind::Dot) {
                    self.bump();
                    if self.at(&TokenKind::LBrace) {
                        break;
                    }
                    path.push(self.ident()?.0);
                } else {
                    break;
                }
            }
            let mut names = Vec::new();
            if self.eat(&TokenKind::LBrace) {
                loop {
                    self.skip_semis();
                    if self.at(&TokenKind::RBrace) {
                        break;
                    }
                    names.push(self.ident()?.0);
                    self.skip_semis();
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RBrace, "`}` to close import list")?;
            }
            let alias = if self.eat(&TokenKind::As) {
                Some(self.ident()?.0)
            } else {
                None
            };
            module.imports.push(Import {
                path,
                alias,
                names,
                span: span.merge(self.prev_span()),
            });
            self.skip_semis();
        }

        while !self.at(&TokenKind::Eof) {
            self.skip_semis();
            if self.at(&TokenKind::Eof) {
                break;
            }
            if self.is_item_macro_invocation() {
                let items = self.parse_item_macro_invocation()?;
                module.items.extend(items);
                self.skip_semis();
                continue;
            }
            let docs = self.collect_docs();
            self.skip_semis();
            if self.at(&TokenKind::Eof) {
                break;
            }
            let mut items = self.parse_item(docs)?;
            module.items.append(&mut items);
            self.skip_semis();
        }
        Ok(module)
    }

    fn collect_docs(&mut self) -> Vec<String> {
        let mut docs = Vec::new();
        loop {
            if let TokenKind::DocComment(text) = self.peek().clone() {
                docs.push(text);
                self.bump();
                self.skip_semis();
            } else {
                break;
            }
        }
        docs
    }

    fn is_item_macro_invocation(&self) -> bool {
        matches!(self.peek(), TokenKind::Ident(_))
            && matches!(self.peek_at(1), TokenKind::Bang)
            && matches!(
                self.peek_at(2),
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace
            )
    }

    fn parse_item_macro_invocation(&mut self) -> Result<Vec<Item>, Diagnostic> {
        let name = match self.bump() {
            TokenKind::Ident(name) => name,
            _ => unreachable!(),
        };
        let start = self.span();
        self.bump(); // !
        let group = self.parse_token_tree()?;
        let tokens = match group {
            TokenTree::Group { tokens, .. } => tokens,
            _ => Vec::new(),
        };
        self.expand_items(&name, &tokens, start)
    }

    // ------------------------------------------------------------------ items

    fn parse_item(&mut self, docs: Vec<String>) -> Result<Vec<Item>, Diagnostic> {
        let attrs = self.parse_attributes()?;
        let start = self.span();
        let vis = if self.eat(&TokenKind::Pub) {
            Visibility::Public
        } else {
            Visibility::Private
        };
        let kind_start = self.pos;
        let kind_span = self.span();
        let kind = match self.peek().clone() {
            TokenKind::Const => {
                self.bump();
                let (name, _) = self.ident()?;
                self.expect(&TokenKind::Colon, "`:` after constant name")?;
                let ty = self.parse_type()?;
                self.expect(&TokenKind::Eq, "`=` in constant definition")?;
                let value = self.parse_expr()?;
                ItemKind::Const { name, ty, value }
            }
            TokenKind::Struct => {
                self.bump();
                let (name, _) = self.ident()?;
                let generics = self.parse_generics()?;
                let fields = self.parse_fields()?;
                ItemKind::Struct {
                    name,
                    generics,
                    fields,
                }
            }
            TokenKind::Enum => {
                self.bump();
                self.parse_enum(false)?
            }
            TokenKind::Open => {
                self.bump();
                self.expect(&TokenKind::Enum, "`enum` after `open`")?;
                self.parse_enum(true)?
            }
            TokenKind::Opaque => {
                self.bump();
                let (name, _) = self.ident()?;
                self.expect(&TokenKind::Eq, "`=` in opaque type")?;
                let ty = self.parse_type()?;
                ItemKind::Opaque { name, ty }
            }
            TokenKind::Type => {
                self.bump();
                let (name, _) = self.ident()?;
                let generics = self.parse_generics()?;
                self.expect(&TokenKind::Eq, "`=` in type alias")?;
                let ty = self.parse_type()?;
                ItemKind::TypeAlias { name, generics, ty }
            }
            TokenKind::Interface => {
                self.bump();
                self.parse_interface()?
            }
            TokenKind::Impl => {
                self.bump();
                self.parse_impl()?
            }
            TokenKind::Fn => {
                self.bump();
                let span = self.span();
                let function = self.parse_function()?;
                let _ = span;
                ItemKind::Fn(function)
            }
            TokenKind::Macro => {
                self.bump();
                let def = self.parse_macro_def()?;
                self.macros.insert(def.name.clone(), def.clone());
                ItemKind::Macro(def)
            }
            // `package`/`import` only valid at the top of a file.
            TokenKind::Package | TokenKind::Import => {
                return Err(self.error("`package` and `import` must appear at the top of the file"))
            }
            other => {
                return Err(self.error_at(self.span(), format!("expected an item, found {other}")))
            }
        };
        let end_span = self.prev_span();
        let item = Item {
            attrs: attrs.clone(),
            vis,
            kind,
            span: start.merge(end_span),
            docs,
        };
        let kind_end = self.pos;
        let _ = kind_span;

        let mut items = vec![item.clone()];
        let derives: Vec<String> = attrs
            .iter()
            .filter(|a| a.name == "derive")
            .flat_map(|a| a.args.clone())
            .collect();
        if !derives.is_empty() {
            let item_tokens = self.tokens[kind_start.min(kind_end)..kind_end].to_vec();
            for derive in derives {
                items.extend(self.expand_derive(&item, &derive, &item_tokens)?);
            }
        }
        Ok(items)
    }

    fn parse_enum(&mut self, open: bool) -> Result<ItemKind, Diagnostic> {
        let (name, _) = self.ident()?;
        let generics = self.parse_generics()?;
        let variants = self.parse_variants()?;
        Ok(ItemKind::Enum {
            name,
            generics,
            open,
            variants,
        })
    }

    fn parse_attributes(&mut self) -> Result<Vec<Attribute>, Diagnostic> {
        let mut attrs = Vec::new();
        while self.at(&TokenKind::Pound) {
            let start = self.span();
            self.bump();
            self.expect(&TokenKind::LBracket, "`[` after `#`")?;
            let (name, _) = self.ident()?;
            let mut args = Vec::new();
            if self.eat(&TokenKind::LParen) {
                loop {
                    self.skip_semis();
                    if self.at(&TokenKind::RParen) {
                        break;
                    }
                    match self.peek().clone() {
                        TokenKind::Ident(arg) => {
                            self.bump();
                            args.push(arg);
                        }
                        _ => {
                            // Unknown attribute payloads are skipped rather
                            // than rejected; they are inert.
                            self.bump();
                        }
                    }
                    self.skip_semis();
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RParen, "`)` to close attribute")?;
            }
            self.expect(&TokenKind::RBracket, "`]` to close attribute")?;
            attrs.push(Attribute {
                name,
                args,
                span: start.merge(self.prev_span()),
            });
            self.skip_semis();
        }
        Ok(attrs)
    }

    fn expand_derive(
        &mut self,
        item: &Item,
        derive: &str,
        item_tokens: &[Token],
    ) -> Result<Vec<Item>, Diagnostic> {
        let name = item
            .kind
            .name()
            .ok_or_else(|| self.error("`#[derive]` is only valid on named items"))?
            .to_string();
        let macro_name = format!("derive_{}", derive.to_ascii_lowercase());
        let span = item.span;

        if self.macros.contains_key(&macro_name) {
            let trees = macros::tree_stream(item_tokens).map_err(|message| {
                self.error_at(span, format!("cannot expand derive macro: {message}"))
            })?;
            return self.expand_items(&macro_name, &trees, span);
        }

        let generics = match &item.kind {
            ItemKind::Struct { generics, .. } => generics.names(),
            ItemKind::Enum { generics, .. } => generics.names(),
            _ => Vec::new(),
        };
        let iface = match derive {
            "Eq" | "Hash" | "Show" | "Clone" | "Debug" => {
                if derive == "Debug" {
                    "Show"
                } else {
                    derive
                }
            }
            other => other,
        };
        if !matches!(iface, "Eq" | "Hash" | "Show" | "Clone") {
            return Err(self
                .error_at(span, format!("no derive macro `{macro_name}!` in scope"))
                .with_note("define a `macro derive_...!` to support this derive"));
        }
        let source = macros::derive_impl_source(&name, &generics, iface);
        let tokens = macros::derive_tokens(&source, span)?;
        self.parse_item_tokens(tokens)
    }

    fn parse_item_tokens(&mut self, tokens: Vec<Token>) -> Result<Vec<Item>, Diagnostic> {
        let mut sub = Parser::new(tokens, self.file, self.macros.clone(), &self.package);
        let mut items = Vec::new();
        sub.skip_semis();
        while !sub.at(&TokenKind::Eof) {
            let docs = sub.collect_docs();
            sub.skip_semis();
            if sub.at(&TokenKind::Eof) {
                break;
            }
            let mut parsed = sub.parse_item(docs)?;
            items.append(&mut parsed);
            sub.skip_semis();
        }
        Ok(items)
    }

    fn parse_fields(&mut self) -> Result<Vec<Field>, Diagnostic> {
        self.expect(&TokenKind::LBrace, "`{` to start field list")?;
        // A trailing `where` clause is accepted here as sugar.
        let mut fields = Vec::new();
        loop {
            self.skip_semis();
            self.skip_docs();
            if self.at(&TokenKind::RBrace) {
                break;
            }
            let (name, span) = self.ident()?;
            self.expect(&TokenKind::Colon, "`:` after field name")?;
            let ty = self.parse_type()?;
            fields.push(Field { name, ty, span });
            self.skip_semis();
            if !self.eat(&TokenKind::Comma) {
                self.skip_semis();
            }
        }
        self.expect(&TokenKind::RBrace, "`}` to close field list")?;
        Ok(fields)
    }

    fn parse_variants(&mut self) -> Result<Vec<Variant>, Diagnostic> {
        self.expect(&TokenKind::LBrace, "`{` to start variant list")?;
        let mut variants = Vec::new();
        loop {
            self.skip_semis();
            self.skip_docs();
            if self.at(&TokenKind::RBrace) {
                break;
            }
            let (name, span) = self.ident()?;
            let mut fields = Vec::new();
            let mut tuple = Vec::new();
            if self.at(&TokenKind::LBrace) {
                fields = self.parse_fields()?;
            } else if self.eat(&TokenKind::LParen) {
                loop {
                    self.skip_semis();
                    if self.at(&TokenKind::RParen) {
                        break;
                    }
                    tuple.push(self.parse_type()?);
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RParen, "`)` to close variant fields")?;
            }
            variants.push(Variant {
                name,
                fields,
                tuple,
                span,
            });
            self.skip_semis();
            if !self.eat(&TokenKind::Comma) {
                self.skip_semis();
            }
        }
        self.expect(&TokenKind::RBrace, "`}` to close enum")?;
        Ok(variants)
    }

    fn parse_interface(&mut self) -> Result<ItemKind, Diagnostic> {
        let (name, _) = self.ident()?;
        let generics = self.parse_generics()?;
        if self.eat(&TokenKind::Colon) {
            // Supertrait bounds are parsed but currently only used for
            // name resolution; they do not participate in lookup.
            while !self.at(&TokenKind::LBrace) && !self.at(&TokenKind::Eof) {
                self.bump();
            }
        }
        self.expect(&TokenKind::LBrace, "`{` to start interface body")?;
        let mut assoc_types = Vec::new();
        let mut methods = Vec::new();
        loop {
            self.skip_semis();
            if self.at(&TokenKind::RBrace) {
                break;
            }
            let docs = self.collect_docs();
            if self.eat(&TokenKind::Type) {
                let (assoc, span) = self.ident()?;
                let mut bounds = Vec::new();
                if self.eat(&TokenKind::Colon) {
                    bounds = self.parse_bounds()?;
                }
                assoc_types.push(AssocType {
                    name: assoc,
                    bounds,
                    span,
                });
                self.skip_semis();
                continue;
            }
            if self.eat(&TokenKind::Fn) {
                let span = self.span();
                let mut method = self.parse_method()?;
                method.docs = docs;
                let _ = span;
                methods.push(method);
                self.skip_semis();
                continue;
            }
            return Err(self.error(format!(
                "expected an associated type or method in interface, found {}",
                self.peek()
            )));
        }
        self.expect(&TokenKind::RBrace, "`}` to close interface")?;
        Ok(ItemKind::Interface {
            name,
            generics,
            assoc_types,
            methods,
        })
    }

    fn parse_impl(&mut self) -> Result<ItemKind, Diagnostic> {
        let generics = self.parse_generics()?;
        let first = self.parse_type()?;
        let (iface, ty) = if self.eat(&TokenKind::For) {
            (Some(first), self.parse_type()?)
        } else {
            (None, first)
        };
        let mut generics = generics;
        self.parse_where_clause(&mut generics)?;
        self.expect(&TokenKind::LBrace, "`{` to start impl body")?;
        let mut assoc = Vec::new();
        let mut methods = Vec::new();
        loop {
            self.skip_semis();
            if self.at(&TokenKind::RBrace) {
                break;
            }
            let docs = self.collect_docs();
            self.skip_semis();
            if self.eat(&TokenKind::Type) {
                let (name, span) = self.ident()?;
                self.expect(&TokenKind::Eq, "`=` in associated type binding")?;
                let ty = self.parse_type()?;
                assoc.push(AssocBinding { name, ty, span });
                self.skip_semis();
                continue;
            }
            if self.eat(&TokenKind::Fn) {
                let mut method = self.parse_method()?;
                method.docs = docs;
                methods.push(method);
                self.skip_semis();
                continue;
            }
            return Err(self.error(format!(
                "expected a method or associated type in impl, found {}",
                self.peek()
            )));
        }
        self.expect(&TokenKind::RBrace, "`}` to close impl")?;
        Ok(ItemKind::Impl {
            generics,
            iface,
            ty,
            assoc,
            methods,
        })
    }

    fn parse_function(&mut self) -> Result<Function, Diagnostic> {
        let (name, name_span) = self.ident()?;
        let mut generics = self.parse_generics()?;
        self.expect(&TokenKind::LParen, "`(` after function name")?;
        let params = self.parse_params()?;
        self.expect(&TokenKind::RParen, "`)` to close parameters")?;
        let ret = if self.eat(&TokenKind::Arrow) {
            Some(self.parse_type()?)
        } else {
            None
        };
        self.parse_where_clause(&mut generics)?;
        let body = if self.eat(&TokenKind::LBrace) {
            Some(self.parse_block_body(name_span)?)
        } else {
            self.eat(&TokenKind::Semi);
            None
        };
        Ok(Function {
            name,
            generics,
            params,
            ret,
            body,
            span: name_span.merge(self.prev_span()),
            docs: Vec::new(),
        })
    }

    fn parse_method(&mut self) -> Result<Method, Diagnostic> {
        let (name, name_span) = self.ident()?;
        let mut generics = self.parse_generics()?;
        self.expect(&TokenKind::LParen, "`(` after method name")?;
        let params = self.parse_params()?;
        self.expect(&TokenKind::RParen, "`)` to close parameters")?;
        let ret = if self.eat(&TokenKind::Arrow) {
            Some(self.parse_type()?)
        } else {
            None
        };
        self.parse_where_clause(&mut generics)?;
        let body = if self.eat(&TokenKind::LBrace) {
            Some(self.parse_block_body(name_span)?)
        } else {
            self.eat(&TokenKind::Semi);
            None
        };
        Ok(Method {
            name,
            generics,
            params,
            ret,
            body,
            span: name_span.merge(self.prev_span()),
            docs: Vec::new(),
        })
    }

    fn parse_params(&mut self) -> Result<Vec<Param>, Diagnostic> {
        let mut params = Vec::new();
        loop {
            self.skip_semis();
            self.skip_docs();
            if self.at(&TokenKind::RParen) || self.at(&TokenKind::Eof) {
                break;
            }
            let start = self.span();
            let mutable = self.eat(&TokenKind::Mut);
            let (name, _) = if self.at(&TokenKind::SelfValue) {
                let span = self.span();
                self.bump();
                ("self".to_string(), span)
            } else {
                self.ident()?
            };
            let ty = if self.eat(&TokenKind::Colon) {
                Some(self.parse_type()?)
            } else if name == "self" {
                Some(TypeExpr::named("Self", start))
            } else {
                return Err(
                    self.error_at(start, format!("parameter `{name}` needs an explicit type"))
                );
            };
            params.push(Param {
                name,
                ty,
                mutable,
                span: start.merge(self.prev_span()),
            });
            self.skip_semis();
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        Ok(params)
    }

    fn parse_generics(&mut self) -> Result<Generics, Diagnostic> {
        let mut params = Vec::new();
        if self.eat(&TokenKind::Lt) {
            loop {
                self.skip_semis();
                if self.at(&TokenKind::Gt) {
                    break;
                }
                let (name, span) = self.ident()?;
                let bounds = if self.eat(&TokenKind::Colon) {
                    self.parse_bounds()?
                } else {
                    Vec::new()
                };
                params.push(GenericParam { name, bounds, span });
                self.skip_semis();
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect_gt("generic parameter list")?;
        }
        let mut generics = Generics { params };
        if self.at(&TokenKind::Where) {
            self.parse_where_clause(&mut generics)?;
        }
        Ok(generics)
    }

    /// `where` clauses are sugar for inline bounds (DESIGN.md §3.5) and are
    /// merged into the generic parameter list during parsing.
    fn parse_where_clause(&mut self, generics: &mut Generics) -> Result<(), Diagnostic> {
        if !self.eat(&TokenKind::Where) {
            return Ok(());
        }
        loop {
            self.skip_semis();
            if self.at(&TokenKind::LBrace) || self.at(&TokenKind::Eq) || self.at(&TokenKind::Eof) {
                break;
            }
            let (name, span) = self.ident()?;
            self.expect(&TokenKind::Colon, "`:` in where clause")?;
            let bounds = self.parse_bounds()?;
            if let Some(param) = generics.params.iter_mut().find(|p| p.name == name) {
                param.bounds.extend(bounds);
            } else {
                generics.params.push(GenericParam { name, bounds, span });
            }
            self.skip_semis();
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        Ok(())
    }

    fn parse_bounds(&mut self) -> Result<Vec<Bound>, Diagnostic> {
        let mut bounds = Vec::new();
        loop {
            let span = self.span();
            let mut path = vec![self.ident()?.0];
            while self.eat(&TokenKind::ColonColon) {
                path.push(self.ident()?.0);
            }
            if path.last().map(String::as_str) == Some("Fn") && self.at(&TokenKind::LParen) {
                self.bump();
                let mut params = Vec::new();
                loop {
                    if self.at(&TokenKind::RParen) {
                        break;
                    }
                    params.push(self.parse_type()?);
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RParen, "`)` in `Fn` bound")?;
                let ret = if self.eat(&TokenKind::Arrow) {
                    self.parse_type()?
                } else {
                    TypeExpr::unit(self.prev_span())
                };
                bounds.push(Bound {
                    path,
                    args: Vec::new(),
                    assoc: Vec::new(),
                    fn_sig: Some((params, Box::new(ret))),
                    span: span.merge(self.prev_span()),
                });
            } else {
                let (args, assoc) = if self.at(&TokenKind::Lt) {
                    self.bump();
                    self.parse_generic_args()?
                } else {
                    (Vec::new(), Vec::new())
                };
                bounds.push(Bound {
                    path,
                    args,
                    assoc,
                    fn_sig: None,
                    span: span.merge(self.prev_span()),
                });
            }
            if !self.eat(&TokenKind::Plus) {
                break;
            }
        }
        Ok(bounds)
    }

    /// Parses `<T, U, Item = V>` after the opening `<`; returns `>` consumed.
    fn parse_generic_args(
        &mut self,
    ) -> Result<(Vec<TypeExpr>, Vec<(String, TypeExpr)>), Diagnostic> {
        let mut args = Vec::new();
        let mut assoc = Vec::new();
        loop {
            self.skip_semis();
            if self.at(&TokenKind::Gt) || self.at(&TokenKind::Shr) {
                break;
            }
            if let TokenKind::Ident(name) = self.peek().clone() {
                if matches!(self.peek_at(1), TokenKind::Eq) {
                    self.bump();
                    self.bump();
                    assoc.push((name, self.parse_type()?));
                } else {
                    args.push(self.parse_type()?);
                }
            } else {
                args.push(self.parse_type()?);
            }
            self.skip_semis();
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect_gt("type argument list")?;
        Ok((args, assoc))
    }

    // ----------------------------------------------------------------- types

    fn parse_type(&mut self) -> Result<TypeExpr, Diagnostic> {
        let start = self.span();
        let kind = match self.peek().clone() {
            TokenKind::Amp => {
                self.bump();
                let mutable = self.eat(&TokenKind::Mut);
                let inner = self.parse_type()?;
                TypeExprKind::Ref {
                    inner: Box::new(inner),
                    mutable,
                }
            }
            TokenKind::LBracket => {
                self.bump();
                let inner = self.parse_type()?;
                self.expect(&TokenKind::RBracket, "`]` after element type")?;
                TypeExprKind::Slice(Box::new(inner))
            }
            TokenKind::LParen => {
                self.bump();
                if self.eat(&TokenKind::RParen) {
                    TypeExprKind::Unit
                } else {
                    let mut items = Vec::new();
                    loop {
                        items.push(self.parse_type()?);
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                        if self.at(&TokenKind::RParen) {
                            break;
                        }
                    }
                    self.expect(&TokenKind::RParen, "`)` to close tuple type")?;
                    if items.len() == 1 {
                        return Ok(items.remove(0));
                    }
                    TypeExprKind::Tuple(items)
                }
            }
            TokenKind::Dyn => {
                self.bump();
                let path = self.parse_path_segments()?;
                let (args, assoc) = if self.at(&TokenKind::Lt) {
                    self.bump();
                    self.parse_generic_args()?
                } else {
                    (Vec::new(), Vec::new())
                };
                TypeExprKind::Dyn { path, args, assoc }
            }
            TokenKind::Ident(name)
                if name == "Fn" && matches!(self.peek_at(1), TokenKind::LParen) =>
            {
                self.bump();
                self.parse_fn_type()?
            }
            TokenKind::Fn => {
                self.bump();
                self.parse_fn_type()?
            }
            TokenKind::Ident(_) => {
                let path = self.parse_path_segments()?;
                let (args, assoc) = if self.at(&TokenKind::Lt) {
                    self.bump();
                    self.parse_generic_args()?
                } else {
                    (Vec::new(), Vec::new())
                };
                TypeExprKind::Path { path, args, assoc }
            }
            other => {
                return Err(self.error(format!("expected a type, found {other}")));
            }
        };
        Ok(TypeExpr::new(kind, start.merge(self.prev_span())))
    }

    fn parse_fn_type(&mut self) -> Result<TypeExprKind, Diagnostic> {
        self.expect(&TokenKind::LParen, "`(` in function type")?;
        let mut params = Vec::new();
        loop {
            if self.at(&TokenKind::RParen) {
                break;
            }
            params.push(self.parse_type()?);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RParen, "`)` in function type")?;
        let ret = if self.eat(&TokenKind::Arrow) {
            self.parse_type()?
        } else {
            TypeExpr::unit(self.prev_span())
        };
        Ok(TypeExprKind::Fn {
            params,
            ret: Box::new(ret),
        })
    }

    fn parse_path_segments(&mut self) -> Result<Vec<String>, Diagnostic> {
        let mut path = vec![self.ident()?.0];
        while self.at(&TokenKind::ColonColon) {
            // `::<` in type position is generic arguments, not a segment.
            if matches!(self.peek_at(1), TokenKind::Lt) {
                break;
            }
            self.bump();
            path.push(self.ident()?.0);
        }
        Ok(path)
    }

    // ------------------------------------------------------------- statements

    fn parse_block_body(&mut self, _owner: Span) -> Result<Block, Diagnostic> {
        let start = self.prev_span();
        let mut stmts = Vec::new();
        loop {
            self.skip_semis();
            if self.at(&TokenKind::RBrace) || self.at(&TokenKind::Eof) {
                break;
            }
            let stmt = self.parse_stmt()?;
            stmts.push(stmt);
        }
        let end = self.prev_span();
        self.expect(&TokenKind::RBrace, "`}` to close block")?;
        Ok(Block {
            stmts,
            span: start.merge(end),
        })
    }

    fn parse_stmt(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.span();
        match self.peek().clone() {
            TokenKind::Let => {
                self.bump();
                let mutable = self.eat(&TokenKind::Mut);
                let pattern = self.parse_pattern()?;
                let ty = if self.eat(&TokenKind::Colon) {
                    Some(self.parse_type()?)
                } else {
                    None
                };
                self.expect(&TokenKind::Eq, "`=` in let binding")?;
                let value = self.parse_expr()?;
                let else_block = if self.eat(&TokenKind::Else) {
                    self.expect(&TokenKind::LBrace, "`{` after `else`")?;
                    Some(self.parse_block_body(start)?)
                } else {
                    None
                };
                let semi = self.eat(&TokenKind::Semi);
                Ok(Stmt {
                    kind: StmtKind::Let {
                        pattern,
                        mutable,
                        ty,
                        value,
                        else_block,
                    },
                    span: start.merge(self.prev_span()),
                    semi,
                })
            }
            TokenKind::Return => {
                self.bump();
                let value = if self.at(&TokenKind::Semi)
                    || self.at(&TokenKind::RBrace)
                    || self.at(&TokenKind::Eof)
                {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                let semi = self.eat(&TokenKind::Semi);
                Ok(Stmt {
                    kind: StmtKind::Return(value),
                    span: start.merge(self.prev_span()),
                    semi,
                })
            }
            TokenKind::Break => {
                self.bump();
                let semi = self.eat(&TokenKind::Semi);
                Ok(Stmt {
                    kind: StmtKind::Break,
                    span: start.merge(self.prev_span()),
                    semi,
                })
            }
            TokenKind::Continue => {
                self.bump();
                let semi = self.eat(&TokenKind::Semi);
                Ok(Stmt {
                    kind: StmtKind::Continue,
                    span: start.merge(self.prev_span()),
                    semi,
                })
            }
            TokenKind::Defer => {
                self.bump();
                let expr = self.parse_expr()?;
                let semi = self.eat(&TokenKind::Semi);
                Ok(Stmt {
                    kind: StmtKind::Defer(expr),
                    span: start.merge(self.prev_span()),
                    semi,
                })
            }
            TokenKind::Fn => {
                // Nested functions are not part of the language; producing a
                // clear error beats mis-parsing the body as an expression.
                return Err(
                    self.error("nested functions are not supported; move it to the package level")
                );
            }
            _ => {
                let expr = self.parse_expr()?;
                let op = self.peek_assignment_op();
                if let Some(op) = op {
                    self.bump();
                    let value = self.parse_expr()?;
                    let semi = self.eat(&TokenKind::Semi);
                    Ok(Stmt {
                        kind: StmtKind::Assign {
                            target: expr,
                            op,
                            value,
                        },
                        span: start.merge(self.prev_span()),
                        semi,
                    })
                } else {
                    let semi = self.eat(&TokenKind::Semi);
                    Ok(Stmt {
                        kind: StmtKind::Expr(expr),
                        span: start.merge(self.prev_span()),
                        semi,
                    })
                }
            }
        }
    }

    fn peek_assignment_op(&self) -> Option<Option<BinOp>> {
        match self.peek() {
            TokenKind::Eq => Some(None),
            TokenKind::PlusEq => Some(Some(BinOp::Add)),
            TokenKind::MinusEq => Some(Some(BinOp::Sub)),
            TokenKind::StarEq => Some(Some(BinOp::Mul)),
            TokenKind::SlashEq => Some(Some(BinOp::Div)),
            TokenKind::PercentEq => Some(Some(BinOp::Rem)),
            _ => None,
        }
    }

    // ---------------------------------------------------------------- patterns

    fn parse_pattern(&mut self) -> Result<Pattern, Diagnostic> {
        let start = self.span();
        let first = self.parse_pattern_single()?;
        if !self.at(&TokenKind::Pipe) {
            return Ok(first);
        }
        let mut alternatives = vec![first];
        while self.eat(&TokenKind::Pipe) {
            alternatives.push(self.parse_pattern_single()?);
        }
        Ok(Pattern::new(
            PatKind::Or(alternatives),
            start.merge(self.prev_span()),
        ))
    }

    fn parse_pattern_single(&mut self) -> Result<Pattern, Diagnostic> {
        let start = self.span();
        match self.peek().clone() {
            TokenKind::Amp => {
                self.bump();
                let mutable = self.eat(&TokenKind::Mut);
                let pattern = self.parse_pattern_single()?;
                Ok(Pattern::new(
                    PatKind::Ref {
                        mutable,
                        pattern: Box::new(pattern),
                    },
                    start.merge(self.prev_span()),
                ))
            }
            TokenKind::LParen => {
                self.bump();
                if self.eat(&TokenKind::RParen) {
                    return Ok(Pattern::new(
                        PatKind::Tuple(Vec::new()),
                        start.merge(self.prev_span()),
                    ));
                }
                let mut items = Vec::new();
                loop {
                    items.push(self.parse_pattern()?);
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                    if self.at(&TokenKind::RParen) {
                        break;
                    }
                }
                self.expect(&TokenKind::RParen, "`)` to close tuple pattern")?;
                if items.len() == 1 {
                    Ok(items.remove(0))
                } else {
                    Ok(Pattern::new(
                        PatKind::Tuple(items),
                        start.merge(self.prev_span()),
                    ))
                }
            }
            TokenKind::Ident(name) if name == "_" => {
                self.bump();
                Ok(Pattern::new(PatKind::Wildcard, start))
            }
            TokenKind::Ident(name) => {
                self.bump();
                if self.at(&TokenKind::ColonColon)
                    || self.at(&TokenKind::LBrace)
                    || self.at(&TokenKind::LParen)
                {
                    let mut path = vec![name];
                    while self.eat(&TokenKind::ColonColon) {
                        path.push(self.ident()?.0);
                    }
                    if self.at(&TokenKind::LBrace) {
                        self.bump();
                        let mut fields = Vec::new();
                        let mut rest = false;
                        loop {
                            self.skip_semis();
                            if self.at(&TokenKind::RBrace) {
                                break;
                            }
                            if self.eat(&TokenKind::DotDot) {
                                rest = true;
                                self.skip_semis();
                                break;
                            }
                            let (field, _) = self.ident()?;
                            if self.eat(&TokenKind::Colon) {
                                let pat = self.parse_pattern()?;
                                fields.push((field, pat));
                            } else {
                                let span = self.prev_span();
                                fields.push((
                                    field.clone(),
                                    Pattern::new(
                                        PatKind::Binding {
                                            name: field,
                                            sub: None,
                                        },
                                        span,
                                    ),
                                ));
                            }
                            self.skip_semis();
                            if !self.eat(&TokenKind::Comma) {
                                break;
                            }
                        }
                        self.expect(&TokenKind::RBrace, "`}` to close struct pattern")?;
                        return Ok(Pattern::new(
                            PatKind::Struct { path, fields, rest },
                            start.merge(self.prev_span()),
                        ));
                    }
                    let mut args = Vec::new();
                    if self.eat(&TokenKind::LParen) {
                        loop {
                            self.skip_semis();
                            if self.at(&TokenKind::RParen) {
                                break;
                            }
                            args.push(self.parse_pattern()?);
                            self.skip_semis();
                            if !self.eat(&TokenKind::Comma) {
                                break;
                            }
                        }
                        self.expect(&TokenKind::RParen, "`)` to close tuple pattern")?;
                    }
                    return Ok(Pattern::new(
                        PatKind::Path { path, args },
                        start.merge(self.prev_span()),
                    ));
                }
                if self.eat(&TokenKind::At) {
                    let sub = self.parse_pattern_single()?;
                    return Ok(Pattern::new(
                        PatKind::Binding {
                            name,
                            sub: Some(Box::new(sub)),
                        },
                        start.merge(self.prev_span()),
                    ));
                }
                // Uppercase single names are treated as unit variants; this is
                // the same convention used for struct/constant names.
                if name.chars().next().is_some_and(char::is_uppercase) {
                    return Ok(Pattern::new(
                        PatKind::Path {
                            path: vec![name],
                            args: Vec::new(),
                        },
                        start,
                    ));
                }
                Ok(Pattern::new(PatKind::Binding { name, sub: None }, start))
            }
            TokenKind::Integer(text) | TokenKind::Float(text) => {
                self.bump();
                self.pattern_literal_or_range(start, text)
            }
            TokenKind::Minus => {
                self.bump();
                match self.peek().clone() {
                    TokenKind::Integer(text) | TokenKind::Float(text) => {
                        self.bump();
                        self.pattern_literal_or_range(start, format!("-{text}"))
                    }
                    _ => Err(self.error("expected a literal after `-` in pattern")),
                }
            }
            TokenKind::Str(value) => {
                self.bump();
                self.pattern_literal_or_range(start, format!("{:?}", value))
            }
            TokenKind::Char(c) => {
                self.bump();
                self.pattern_literal_or_range(start, format!("{:?}", c))
            }
            TokenKind::True | TokenKind::False => {
                let value = matches!(self.bump(), TokenKind::True);
                self.pattern_literal_or_range(start, value.to_string())
            }
            other => Err(self.error(format!("expected a pattern, found {other}"))),
        }
    }

    fn pattern_literal_or_range(
        &mut self,
        start: Span,
        text: String,
    ) -> Result<Pattern, Diagnostic> {
        let lo = parse_pat_literal(&text)
            .ok_or_else(|| self.error_at(start, format!("invalid literal pattern `{text}`")))?;
        if self.at(&TokenKind::DotDot) || self.at(&TokenKind::DotDotEq) {
            let inclusive = self.at(&TokenKind::DotDotEq);
            self.bump();
            let high_text = match self.peek().clone() {
                TokenKind::Integer(t) | TokenKind::Float(t) => {
                    self.bump();
                    t
                }
                TokenKind::Str(v) => {
                    self.bump();
                    format!("{:?}", v)
                }
                TokenKind::Char(c) => {
                    self.bump();
                    format!("{:?}", c)
                }
                other => {
                    return Err(self.error(format!(
                        "expected a literal after range operator, found {other}"
                    )))
                }
            };
            let hi = parse_pat_literal(&high_text).ok_or_else(|| {
                self.error_at(start, format!("invalid literal pattern `{high_text}`"))
            })?;
            return Ok(Pattern::new(
                PatKind::Range { lo, hi, inclusive },
                start.merge(self.prev_span()),
            ));
        }
        Ok(Pattern::new(PatKind::Literal(lo), start))
    }

    // ------------------------------------------------------------- expressions

    fn parse_expr(&mut self) -> Result<Expr, Diagnostic> {
        self.parse_expr_bp(0)
    }

    fn parse_expr_bp(&mut self, min_bp: u8) -> Result<Expr, Diagnostic> {
        let start = self.span();
        let mut lhs = self.parse_unary()?;
        loop {
            // Range expressions bind loosest.
            if (self.at(&TokenKind::DotDot) || self.at(&TokenKind::DotDotEq)) && min_bp == 0 {
                let inclusive = self.at(&TokenKind::DotDotEq);
                self.bump();
                let hi = if self.expr_can_start() {
                    Some(Box::new(self.parse_expr_bp(1)?))
                } else {
                    None
                };
                lhs = Expr::new(
                    ExprKind::Range {
                        lo: Some(Box::new(lhs)),
                        hi,
                        inclusive,
                    },
                    start.merge(self.prev_span()),
                );
                continue;
            }
            let Some((op, prec)) = self.peek_binop() else {
                break;
            };
            if prec < min_bp {
                break;
            }
            self.bump();
            let rhs = self.parse_expr_bp(prec + 1)?;
            lhs = Expr::new(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                start.merge(self.prev_span()),
            );
        }
        Ok(lhs)
    }

    fn expr_can_start(&self) -> bool {
        matches!(
            self.peek(),
            TokenKind::Integer(_)
                | TokenKind::Float(_)
                | TokenKind::Str(_)
                | TokenKind::Char(_)
                | TokenKind::True
                | TokenKind::False
                | TokenKind::Ident(_)
                | TokenKind::SelfValue
                | TokenKind::LParen
                | TokenKind::LBracket
                | TokenKind::LBrace
                | TokenKind::Minus
                | TokenKind::Bang
                | TokenKind::Amp
                | TokenKind::If
                | TokenKind::Match
                | TokenKind::Loop
                | TokenKind::While
                | TokenKind::For
                | TokenKind::Try
                | TokenKind::Pipe
                | TokenKind::PipePipe
                | TokenKind::DotDot
                | TokenKind::DotDotEq
        )
    }

    fn peek_binop(&self) -> Option<(BinOp, u8)> {
        let op = match self.peek() {
            TokenKind::Plus => BinOp::Add,
            TokenKind::Minus => BinOp::Sub,
            TokenKind::Star => BinOp::Mul,
            TokenKind::Slash => BinOp::Div,
            TokenKind::Percent => BinOp::Rem,
            TokenKind::Shl => BinOp::Shl,
            TokenKind::Shr => BinOp::Shr,
            TokenKind::Amp => BinOp::BitAnd,
            TokenKind::Pipe => BinOp::BitOr,
            TokenKind::Caret => BinOp::BitXor,
            TokenKind::EqEq => BinOp::Eq,
            TokenKind::BangEq => BinOp::Ne,
            TokenKind::Lt => BinOp::Lt,
            TokenKind::LtEq => BinOp::Le,
            TokenKind::Gt => BinOp::Gt,
            TokenKind::GtEq => BinOp::Ge,
            TokenKind::AmpAmp => BinOp::AndAnd,
            TokenKind::PipePipe => BinOp::OrOr,
            _ => return None,
        };
        Some((op, op.precedence()))
    }

    fn parse_unary(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.span();
        match self.peek().clone() {
            TokenKind::Minus => {
                self.bump();
                let expr = self.parse_unary()?;
                Ok(Expr::new(
                    ExprKind::Unary {
                        op: UnOp::Neg,
                        expr: Box::new(expr),
                    },
                    start.merge(self.prev_span()),
                ))
            }
            TokenKind::Bang => {
                self.bump();
                let expr = self.parse_unary()?;
                Ok(Expr::new(
                    ExprKind::Unary {
                        op: UnOp::Not,
                        expr: Box::new(expr),
                    },
                    start.merge(self.prev_span()),
                ))
            }
            TokenKind::Amp => {
                self.bump();
                let mutable = self.eat(&TokenKind::Mut);
                let expr = self.parse_unary()?;
                Ok(Expr::new(
                    ExprKind::Unary {
                        op: if mutable { UnOp::RefMut } else { UnOp::Ref },
                        expr: Box::new(expr),
                    },
                    start.merge(self.prev_span()),
                ))
            }
            TokenKind::DotDot | TokenKind::DotDotEq => {
                let inclusive = self.at(&TokenKind::DotDotEq);
                self.bump();
                let hi = if self.expr_can_start() {
                    Some(Box::new(self.parse_expr_bp(1)?))
                } else {
                    None
                };
                Ok(Expr::new(
                    ExprKind::Range {
                        lo: None,
                        hi,
                        inclusive,
                    },
                    start.merge(self.prev_span()),
                ))
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.span();
        let mut expr = self.parse_primary()?;
        loop {
            match self.peek().clone() {
                TokenKind::LParen => {
                    self.bump();
                    let saved = self.no_struct_lit;
                    self.no_struct_lit = false;
                    let mut args = Vec::new();
                    loop {
                        self.skip_semis();
                        if self.at(&TokenKind::RParen) {
                            break;
                        }
                        args.push(self.parse_expr()?);
                        self.skip_semis();
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    self.no_struct_lit = saved;
                    self.expect(&TokenKind::RParen, "`)` to close call")?;
                    expr = Expr::new(
                        ExprKind::Call {
                            callee: Box::new(expr),
                            args,
                        },
                        start.merge(self.prev_span()),
                    );
                }
                TokenKind::Dot => {
                    self.bump();
                    let (name, member_span) = match self.peek().clone() {
                        TokenKind::Ident(name) => {
                            self.bump();
                            (name, self.prev_span())
                        }
                        TokenKind::Integer(text) if text.chars().all(|c| c.is_ascii_digit()) => {
                            self.bump();
                            (text, self.prev_span())
                        }
                        other => {
                            return Err(self.error(format!(
                                "expected a field or method name after `.`, found {other}"
                            )))
                        }
                    };
                    let mut type_args = Vec::new();
                    if self.at(&TokenKind::ColonColon) && matches!(self.peek_at(1), TokenKind::Lt) {
                        self.bump();
                        self.bump();
                        let (args, _assoc) = self.parse_generic_args()?;
                        type_args = args;
                    }
                    if self.at(&TokenKind::LParen) {
                        if !type_args.is_empty() {
                            // Turbofish arguments on methods are accepted; the
                            // checker infers them like function calls.
                        }
                        self.bump();
                        let saved = self.no_struct_lit;
                        self.no_struct_lit = false;
                        let mut args = Vec::new();
                        loop {
                            self.skip_semis();
                            if self.at(&TokenKind::RParen) {
                                break;
                            }
                            args.push(self.parse_expr()?);
                            self.skip_semis();
                            if !self.eat(&TokenKind::Comma) {
                                break;
                            }
                        }
                        self.no_struct_lit = saved;
                        self.expect(&TokenKind::RParen, "`)` to close method call")?;
                        expr = Expr::new(
                            ExprKind::MethodCall {
                                receiver: Box::new(expr),
                                name,
                                args,
                                span: member_span.merge(self.prev_span()),
                            },
                            start.merge(self.prev_span()),
                        );
                    } else {
                        expr = Expr::new(
                            ExprKind::Field {
                                base: Box::new(expr),
                                name,
                            },
                            start.merge(self.prev_span()),
                        );
                    }
                }
                TokenKind::LBracket => {
                    self.bump();
                    let index = self.parse_expr()?;
                    self.expect(&TokenKind::RBracket, "`]` to close index")?;
                    expr = Expr::new(
                        ExprKind::Index {
                            base: Box::new(expr),
                            index: Box::new(index),
                        },
                        start.merge(self.prev_span()),
                    );
                }
                TokenKind::Question => {
                    self.bump();
                    expr = Expr::new(ExprKind::Try(Box::new(expr)), start.merge(self.prev_span()));
                }
                TokenKind::As => {
                    self.bump();
                    let ty = self.parse_type()?;
                    expr = Expr::new(
                        ExprKind::Cast {
                            expr: Box::new(expr),
                            ty,
                        },
                        start.merge(self.prev_span()),
                    );
                }
                TokenKind::LBrace
                    if !self.no_struct_lit && matches!(&expr.kind, ExprKind::Path { .. }) =>
                {
                    let path_expr = expr.kind.clone();
                    let ExprKind::Path { path, args } = path_expr else {
                        unreachable!()
                    };
                    let lit_span = self.span();
                    self.bump();
                    let mut fields = Vec::new();
                    loop {
                        self.skip_semis();
                        if self.at(&TokenKind::RBrace) {
                            break;
                        }
                        let (field, field_span) = self.ident()?;
                        if self.eat(&TokenKind::Colon) {
                            let value = self.parse_expr()?;
                            fields.push((field, value));
                        } else {
                            fields.push((
                                field.clone(),
                                Expr::new(
                                    ExprKind::Path {
                                        path: vec![field],
                                        args: Vec::new(),
                                    },
                                    field_span,
                                ),
                            ));
                        }
                        self.skip_semis();
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect(&TokenKind::RBrace, "`}` to close struct literal")?;
                    expr = Expr::new(
                        ExprKind::StructLit {
                            path,
                            args,
                            fields,
                            span: lit_span.merge(self.prev_span()),
                        },
                        start.merge(self.prev_span()),
                    );
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.span();
        let kind = match self.peek().clone() {
            TokenKind::Integer(text) => {
                self.bump();
                let (value, suffix) = parse_int_literal(&text).ok_or_else(|| {
                    self.error_at(start, format!("invalid integer literal `{text}`"))
                })?;
                ExprKind::Int {
                    text,
                    value,
                    suffix,
                }
            }
            TokenKind::Float(text) => {
                self.bump();
                let (value, suffix) = parse_float_literal(&text).ok_or_else(|| {
                    self.error_at(start, format!("invalid float literal `{text}`"))
                })?;
                ExprKind::Float {
                    text,
                    value,
                    suffix,
                }
            }
            TokenKind::Str(value) => {
                self.bump();
                ExprKind::Str(value)
            }
            TokenKind::Char(c) => {
                self.bump();
                ExprKind::Char(c)
            }
            TokenKind::True => {
                self.bump();
                ExprKind::Bool(true)
            }
            TokenKind::False => {
                self.bump();
                ExprKind::Bool(false)
            }
            TokenKind::SelfValue => {
                self.bump();
                ExprKind::SelfValue
            }
            TokenKind::Ident(name) => {
                self.bump();
                if self.at(&TokenKind::Bang) {
                    self.bump();
                    let group = self.parse_token_tree()?;
                    let tokens = match group {
                        TokenTree::Group { tokens, .. } => tokens,
                        TokenTree::Token(tok) => vec![TokenTree::Token(tok)],
                    };
                    return self.expand_expr(&name, &tokens, start);
                }
                let mut path = vec![name];
                let mut args = Vec::new();
                while self.at(&TokenKind::ColonColon) {
                    self.bump();
                    if self.at(&TokenKind::Lt) {
                        self.bump();
                        let (parsed, _) = self.parse_generic_args()?;
                        args = parsed;
                        continue;
                    }
                    path.push(self.ident()?.0);
                }
                ExprKind::Path { path, args }
            }
            TokenKind::LParen => {
                self.bump();
                let saved = self.no_struct_lit;
                self.no_struct_lit = false;
                let mut items = Vec::new();
                loop {
                    self.skip_semis();
                    if self.at(&TokenKind::RParen) {
                        break;
                    }
                    items.push(self.parse_expr()?);
                    self.skip_semis();
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.no_struct_lit = saved;
                self.expect(&TokenKind::RParen, "`)` to close parenthesized expression")?;
                if items.len() == 1 {
                    return Ok(items.remove(0));
                }
                ExprKind::Tuple(items)
            }
            TokenKind::LBracket => {
                self.bump();
                let saved = self.no_struct_lit;
                self.no_struct_lit = false;
                let mut items = Vec::new();
                loop {
                    self.skip_semis();
                    if self.at(&TokenKind::RBracket) {
                        break;
                    }
                    items.push(self.parse_expr()?);
                    self.skip_semis();
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.no_struct_lit = saved;
                self.expect(&TokenKind::RBracket, "`]` to close list")?;
                ExprKind::List(items)
            }
            TokenKind::LBrace => {
                self.bump();
                ExprKind::Block(self.parse_block_body(start)?)
            }
            TokenKind::If => {
                self.bump();
                if self.eat(&TokenKind::Let) {
                    let pattern = self.parse_pattern()?;
                    self.expect(&TokenKind::Eq, "`=` in `if let`")?;
                    let saved = self.no_struct_lit;
                    self.no_struct_lit = true;
                    let scrutinee = self.parse_expr()?;
                    self.no_struct_lit = saved;
                    self.expect(&TokenKind::LBrace, "`{` after `if let` condition")?;
                    let then_branch = self.parse_block_body(start)?;
                    let else_branch = self.parse_else_branch()?;
                    ExprKind::IfLet {
                        pattern,
                        scrutinee: Box::new(scrutinee),
                        then_branch,
                        else_branch,
                    }
                } else {
                    let saved = self.no_struct_lit;
                    self.no_struct_lit = true;
                    let cond = self.parse_expr()?;
                    self.no_struct_lit = saved;
                    self.expect(&TokenKind::LBrace, "`{` after `if` condition")?;
                    let then_branch = self.parse_block_body(start)?;
                    let else_branch = self.parse_else_branch()?;
                    ExprKind::If {
                        cond: Box::new(cond),
                        then_branch,
                        else_branch,
                    }
                }
            }
            TokenKind::While => {
                self.bump();
                if self.eat(&TokenKind::Let) {
                    let pattern = self.parse_pattern()?;
                    self.expect(&TokenKind::Eq, "`=` in `while let`")?;
                    let saved = self.no_struct_lit;
                    self.no_struct_lit = true;
                    let scrutinee = self.parse_expr()?;
                    self.no_struct_lit = saved;
                    self.expect(&TokenKind::LBrace, "`{` after `while let` condition")?;
                    let body = self.parse_block_body(start)?;
                    ExprKind::WhileLet {
                        pattern,
                        scrutinee: Box::new(scrutinee),
                        body,
                    }
                } else {
                    let saved = self.no_struct_lit;
                    self.no_struct_lit = true;
                    let cond = self.parse_expr()?;
                    self.no_struct_lit = saved;
                    self.expect(&TokenKind::LBrace, "`{` after `while` condition")?;
                    let body = self.parse_block_body(start)?;
                    ExprKind::While {
                        cond: Box::new(cond),
                        body,
                    }
                }
            }
            TokenKind::Loop => {
                self.bump();
                self.expect(&TokenKind::LBrace, "`{` after `loop`")?;
                ExprKind::Loop {
                    body: self.parse_block_body(start)?,
                }
            }
            TokenKind::For => {
                self.bump();
                let pattern = self.parse_pattern()?;
                self.expect(&TokenKind::In, "`in` in `for` loop")?;
                let saved = self.no_struct_lit;
                self.no_struct_lit = true;
                let iter = self.parse_expr()?;
                self.no_struct_lit = saved;
                self.expect(&TokenKind::LBrace, "`{` after `for` iterable")?;
                let body = self.parse_block_body(start)?;
                ExprKind::For {
                    pattern,
                    iter: Box::new(iter),
                    body,
                }
            }
            TokenKind::Match => {
                self.bump();
                let saved = self.no_struct_lit;
                self.no_struct_lit = true;
                let scrutinee = self.parse_expr()?;
                self.no_struct_lit = saved;
                self.expect(&TokenKind::LBrace, "`{` after `match` scrutinee")?;
                let arms = self.parse_match_arms()?;
                ExprKind::Match {
                    scrutinee: Box::new(scrutinee),
                    arms,
                }
            }
            TokenKind::Try => {
                self.bump();
                self.expect(&TokenKind::LBrace, "`{` after `try`")?;
                ExprKind::Block(self.parse_block_body(start)?)
            }
            TokenKind::Pipe => {
                self.bump();
                let mut params = Vec::new();
                loop {
                    self.skip_semis();
                    if self.at(&TokenKind::Pipe) {
                        break;
                    }
                    let param_start = self.span();
                    let (name, _) = self.ident()?;
                    let ty = if self.eat(&TokenKind::Colon) {
                        Some(self.parse_type()?)
                    } else {
                        None
                    };
                    params.push(Param {
                        name,
                        ty,
                        mutable: false,
                        span: param_start.merge(self.prev_span()),
                    });
                    self.skip_semis();
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::Pipe, "`|` to close closure parameters")?;
                let ret = if self.eat(&TokenKind::Arrow) {
                    Some(self.parse_type()?)
                } else {
                    None
                };
                let body = self.parse_expr()?;
                ExprKind::Closure {
                    params,
                    ret,
                    body: Box::new(body),
                }
            }
            TokenKind::PipePipe => {
                self.bump();
                let ret = if self.eat(&TokenKind::Arrow) {
                    Some(self.parse_type()?)
                } else {
                    None
                };
                let body = self.parse_expr()?;
                ExprKind::Closure {
                    params: Vec::new(),
                    ret,
                    body: Box::new(body),
                }
            }
            other => {
                return Err(self.error(format!("expected an expression, found {other}")));
            }
        };
        Ok(Expr::new(kind, start.merge(self.prev_span())))
    }

    fn parse_else_branch(&mut self) -> Result<Option<Box<Expr>>, Diagnostic> {
        if !self.eat(&TokenKind::Else) {
            return Ok(None);
        }
        if self.at(&TokenKind::If) {
            let start = self.span();
            self.bump();
            // Reuse the `if` parser.
            if self.eat(&TokenKind::Let) {
                let pattern = self.parse_pattern()?;
                self.expect(&TokenKind::Eq, "`=` in `else if let`")?;
                let saved = self.no_struct_lit;
                self.no_struct_lit = true;
                let scrutinee = self.parse_expr()?;
                self.no_struct_lit = saved;
                self.expect(&TokenKind::LBrace, "`{` after `else if let` condition")?;
                let then_branch = self.parse_block_body(start)?;
                let else_branch = self.parse_else_branch()?;
                return Ok(Some(Box::new(Expr::new(
                    ExprKind::IfLet {
                        pattern,
                        scrutinee: Box::new(scrutinee),
                        then_branch,
                        else_branch,
                    },
                    start.merge(self.prev_span()),
                ))));
            }
            let saved = self.no_struct_lit;
            self.no_struct_lit = true;
            let cond = self.parse_expr()?;
            self.no_struct_lit = saved;
            self.expect(&TokenKind::LBrace, "`{` after `else if` condition")?;
            let then_branch = self.parse_block_body(start)?;
            let else_branch = self.parse_else_branch()?;
            return Ok(Some(Box::new(Expr::new(
                ExprKind::If {
                    cond: Box::new(cond),
                    then_branch,
                    else_branch,
                },
                start.merge(self.prev_span()),
            ))));
        }
        self.expect(&TokenKind::LBrace, "`{` after `else`")?;
        let block = self.parse_block_body(self.prev_span())?;
        Ok(Some(Box::new(Expr::new(
            ExprKind::Block(block.clone()),
            block.span,
        ))))
    }

    fn parse_match_arms(&mut self) -> Result<Vec<MatchArm>, Diagnostic> {
        let mut arms = Vec::new();
        loop {
            self.skip_semis();
            if self.at(&TokenKind::RBrace) || self.at(&TokenKind::Eof) {
                break;
            }
            let start = self.span();
            let pattern = self.parse_pattern()?;
            let guard = if self.eat(&TokenKind::If) {
                Some(self.parse_expr()?)
            } else {
                None
            };
            self.expect(&TokenKind::FatArrow, "`=>` in match arm")?;
            let body = self.parse_expr()?;
            arms.push(MatchArm {
                pattern,
                guard,
                body,
                span: start.merge(self.prev_span()),
            });
            self.skip_semis();
            self.eat(&TokenKind::Comma);
        }
        self.expect(&TokenKind::RBrace, "`}` to close match")?;
        Ok(arms)
    }

    // ---------------------------------------------------------------- macros

    fn parse_token_tree(&mut self) -> Result<TokenTree, Diagnostic> {
        match self.peek().clone() {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                let open = self.bump();
                let delim = match open {
                    TokenKind::LParen => Delim::Paren,
                    TokenKind::LBracket => Delim::Bracket,
                    _ => Delim::Brace,
                };
                let start = self.prev_span();
                let mut tokens = Vec::new();
                while !self.at(&delim.close()) && !self.at(&TokenKind::Eof) {
                    tokens.push(self.parse_token_tree()?);
                }
                let end = self.expect(&delim.close(), "closing delimiter")?;
                Ok(TokenTree::Group {
                    delim,
                    tokens,
                    span: start.merge(end),
                })
            }
            TokenKind::Eof => Err(self.error("unexpected end of file")),
            _ => {
                let span = self.span();
                let kind = self.bump();
                Ok(TokenTree::Token(Token { kind, span }))
            }
        }
    }

    fn expand_expr(
        &mut self,
        name: &str,
        input: &[TokenTree],
        span: Span,
    ) -> Result<Expr, Diagnostic> {
        let expanded = self.expand_tokens(name, input, span)?;
        if self.expansion_depth > 128 {
            return Err(self.error_at(span, "macro expansion is too deep"));
        }
        let tokens = macros::flatten_trees(&expanded, self.file);
        let mut sub = Parser::new(tokens, self.file, self.macros.clone(), &self.package);
        sub.expansion_depth = self.expansion_depth + 1;
        let expr = sub.parse_expr()?;
        if !sub.at(&TokenKind::Eof) {
            return Err(sub.error(format!(
                "unexpected tokens after macro expansion: {}",
                sub.peek()
            )));
        }
        Ok(expr)
    }

    fn expand_items(
        &mut self,
        name: &str,
        input: &[TokenTree],
        span: Span,
    ) -> Result<Vec<Item>, Diagnostic> {
        let expanded = self.expand_tokens(name, input, span)?;
        let tokens = macros::flatten_trees(&expanded, self.file);
        let mut sub = Parser::new(tokens, self.file, self.macros.clone(), &self.package);
        sub.expansion_depth = self.expansion_depth + 1;
        let mut items = Vec::new();
        sub.skip_semis();
        while !sub.at(&TokenKind::Eof) {
            let docs = sub.collect_docs();
            sub.skip_semis();
            if sub.at(&TokenKind::Eof) {
                break;
            }
            let mut parsed = sub.parse_item(docs)?;
            items.append(&mut parsed);
            sub.skip_semis();
        }
        Ok(items)
    }

    fn expand_tokens(
        &mut self,
        name: &str,
        input: &[TokenTree],
        span: Span,
    ) -> Result<Vec<TokenTree>, Diagnostic> {
        let mut expander = Expander::new(&self.macros);
        expander.depth = self.expansion_depth;
        let result = expander.expand(name, input, span, &self.package);
        self.expansion_depth = expander.depth;
        result
    }

    fn parse_macro_def(&mut self) -> Result<MacroDef, Diagnostic> {
        let (name, span) = self.ident()?;
        self.eat(&TokenKind::Bang);
        self.expect(&TokenKind::LBrace, "`{` to start macro definition")?;
        let mut rules = Vec::new();
        loop {
            self.skip_semis();
            if self.at(&TokenKind::RBrace) || self.at(&TokenKind::Eof) {
                break;
            }
            let rule_start = self.span();
            let matcher_tree = self.parse_token_tree()?;
            let matcher = match matcher_tree {
                TokenTree::Group { tokens, .. } => tokens
                    .into_iter()
                    .filter(|t| !t.is_kind(&TokenKind::Semi))
                    .collect(),
                _ => {
                    return Err(self.error_at(rule_start, "macro matcher must be a delimited group"))
                }
            };
            self.skip_semis();
            self.expect(&TokenKind::FatArrow, "`=>` in macro rule")?;
            self.skip_semis();
            let transcriber_tree = self.parse_token_tree()?;
            let transcriber = match transcriber_tree {
                TokenTree::Group { tokens, .. } => tokens,
                TokenTree::Token(tok) => vec![TokenTree::Token(tok)],
            };
            rules.push(MacroRule {
                matcher,
                transcriber,
                span: rule_start.merge(self.prev_span()),
            });
            self.skip_semis();
            self.eat(&TokenKind::Comma);
        }
        self.expect(&TokenKind::RBrace, "`}` to close macro definition")?;
        Ok(MacroDef {
            name,
            rules,
            span: span.merge(self.prev_span()),
            package: self.package.clone(),
            builtin: false,
        })
    }
}

fn parse_pat_literal(text: &str) -> Option<PatLiteral> {
    if text == "true" {
        return Some(PatLiteral::Bool(true));
    }
    if text == "false" {
        return Some(PatLiteral::Bool(false));
    }
    if let Some(rest) = text.strip_prefix('"') {
        return Some(PatLiteral::Str(rest.trim_end_matches('"').to_string()));
    }
    if let Some(rest) = text.strip_prefix('\'') {
        if let Some(c) = rest.trim_end_matches('\'').chars().next() {
            return Some(PatLiteral::Char(c));
        }
    }
    if text.contains('.') {
        return text
            .parse::<f64>()
            .ok()
            .map(|v| PatLiteral::Float(text.to_string(), v));
    }
    parse_int_literal(text).map(|(v, _)| PatLiteral::Int(text.to_string(), v))
}
