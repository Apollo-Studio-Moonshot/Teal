//! Declarative, hygienic macros over token trees.
//!
//! Expansion is pure token rewriting: macros never see types, never run
//! arbitrary code, and are memoizable by `(definition digest, argument digest)`.
//! Hygiene is implemented by lexical marks (`name#mark`) applied to bindings
//! introduced by the template, while tokens substituted from the call site are
//! left untouched so they cannot be captured.

use crate::lexer::{lex, Token, TokenKind};
use crate::{codes, Diagnostic, FileId, Span};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delim {
    Paren,
    Bracket,
    Brace,
}

impl Delim {
    pub fn open(self) -> TokenKind {
        match self {
            Delim::Paren => TokenKind::LParen,
            Delim::Bracket => TokenKind::LBracket,
            Delim::Brace => TokenKind::LBrace,
        }
    }

    pub fn close(self) -> TokenKind {
        match self {
            Delim::Paren => TokenKind::RParen,
            Delim::Bracket => TokenKind::RBracket,
            Delim::Brace => TokenKind::RBrace,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenTree {
    Token(Token),
    Group {
        delim: Delim,
        tokens: Vec<TokenTree>,
        span: Span,
    },
}

impl TokenTree {
    pub fn span(&self) -> Span {
        match self {
            TokenTree::Token(tok) => tok.span,
            TokenTree::Group { span, .. } => *span,
        }
    }

    pub fn token(&self) -> Option<&Token> {
        match self {
            TokenTree::Token(tok) => Some(tok),
            _ => None,
        }
    }

    pub fn is_kind(&self, kind: &TokenKind) -> bool {
        matches!(self.token(), Some(tok) if &tok.kind == kind)
    }

    pub fn is_ident(&self, name: &str) -> bool {
        matches!(self.token(), Some(tok) if tok.kind == TokenKind::Ident(name.into()))
    }
}

impl Delim {
    fn from_open(kind: &TokenKind) -> Option<Delim> {
        match kind {
            TokenKind::LParen => Some(Delim::Paren),
            TokenKind::LBracket => Some(Delim::Bracket),
            TokenKind::LBrace => Some(Delim::Brace),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MacroDef {
    pub name: String,
    pub rules: Vec<MacroRule>,
    pub span: Span,
    pub package: String,
    pub builtin: bool,
}

#[derive(Clone, Debug)]
pub struct MacroRule {
    pub matcher: Vec<TokenTree>,
    pub transcriber: Vec<TokenTree>,
    pub span: Span,
}

/// Convert a flat token stream (as produced by the lexer) into a tree with
/// explicit delimiter groups. The Eof token is dropped.
pub fn tree_stream(tokens: &[Token]) -> Result<Vec<TokenTree>, String> {
    let mut pos = 0;
    let (trees, _) = group_until(tokens, &mut pos, None)?;
    Ok(trees)
}

fn group_until(
    tokens: &[Token],
    pos: &mut usize,
    closing: Option<TokenKind>,
) -> Result<(Vec<TokenTree>, Span), String> {
    let mut out = Vec::new();
    while *pos < tokens.len() {
        let tok = &tokens[*pos];
        if tok.kind == TokenKind::Eof {
            break;
        }
        if let Some(close) = &closing {
            if &tok.kind == close {
                *pos += 1;
                return Ok((out, tok.span));
            }
        }
        if let Some(delim) = Delim::from_open(&tok.kind) {
            let open_span = tok.span;
            *pos += 1;
            let (inner, close_span) = group_until(tokens, pos, Some(delim.close()))?;
            out.push(TokenTree::Group {
                delim,
                tokens: inner,
                span: open_span.merge(close_span),
            });
        } else {
            if matches!(
                tok.kind,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace
            ) {
                return Err(format!(
                    "unexpected closing delimiter `{}`",
                    crate::lexer::token_text(&tok.kind)
                ));
            }
            out.push(TokenTree::Token(tok.clone()));
            *pos += 1;
        }
    }
    if closing.is_some() {
        return Err("unterminated delimiter".into());
    }
    Ok((out, Span::dummy()))
}

pub fn flatten(trees: &[TokenTree], out: &mut Vec<Token>) {
    for tree in trees {
        match tree {
            TokenTree::Token(tok) => out.push(tok.clone()),
            TokenTree::Group {
                delim,
                tokens,
                span,
            } => {
                out.push(Token {
                    kind: delim.open(),
                    span: *span,
                });
                flatten(tokens, out);
                out.push(Token {
                    kind: delim.close(),
                    span: *span,
                });
            }
        }
    }
}

pub fn flatten_trees(trees: &[TokenTree], file: FileId) -> Vec<Token> {
    let mut out = Vec::new();
    flatten(trees, &mut out);
    out.push(Token {
        kind: TokenKind::Eof,
        span: trees
            .last()
            .map(|t| t.span())
            .unwrap_or(Span::new(file, 0, 0)),
    });
    out
}

/// Parse a macro definition from source fragments. Used to construct the
/// prelude's bundled macros without special-casing them in the parser.
pub fn macro_from_source(
    name: &str,
    rules: &[(&str, &str)],
    file: FileId,
    package: &str,
) -> MacroDef {
    let owned: Vec<(String, String)> = rules
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    macro_def_from_rules(name, &owned, file, package)
}

/// Reconstruct a macro definition from rule source fragments stored in a
/// `.tli` summary.
pub fn macro_def_from_rules(
    name: &str,
    rules: &[(String, String)],
    file: FileId,
    package: &str,
) -> MacroDef {
    let mut parsed = Vec::new();
    for (matcher, transcriber) in rules {
        let matcher_tokens = lex(&format!("({matcher})"), file).unwrap_or_default();
        let matcher_trees = tree_stream(&matcher_tokens).unwrap_or_default();
        let matcher = match matcher_trees.into_iter().next() {
            Some(TokenTree::Group { tokens, .. }) => strip_semis(tokens),
            _ => Vec::new(),
        };
        let trans_tokens = lex(&format!("{{{transcriber}}}"), file).unwrap_or_default();
        let trans_trees = tree_stream(&trans_tokens).unwrap_or_default();
        let transcriber = match trans_trees.into_iter().next() {
            Some(TokenTree::Group { tokens, .. }) => tokens,
            _ => Vec::new(),
        };
        parsed.push(MacroRule {
            matcher,
            transcriber,
            span: Span::dummy(),
        });
    }
    MacroDef {
        name: name.to_string(),
        rules: parsed,
        span: Span::dummy(),
        package: package.to_string(),
        builtin: false,
    }
}

/// Render a token tree back to source-like text. Used to store macro rules in
/// interface summaries so downstream packages can expand them.
pub fn render_tokens(tokens: &[TokenTree]) -> String {
    let mut out = String::new();
    for token in tokens {
        render_token(token, &mut out);
        out.push(' ');
    }
    out.trim_end().to_string()
}

fn render_token(token: &TokenTree, out: &mut String) {
    match token {
        TokenTree::Token(tok) => match &tok.kind {
            TokenKind::Ident(name) => out.push_str(name),
            TokenKind::Integer(text) | TokenKind::Float(text) => out.push_str(text),
            TokenKind::Str(value) => {
                out.push('"');
                for c in value.chars() {
                    match c {
                        '\n' => out.push_str("\\n"),
                        '\t' => out.push_str("\\t"),
                        '\r' => out.push_str("\\r"),
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        other => out.push(other),
                    }
                }
                out.push('"');
            }
            TokenKind::Char(c) => {
                out.push('\'');
                match c {
                    '\n' => out.push_str("\\n"),
                    '\t' => out.push_str("\\t"),
                    '\'' => out.push_str("\\'"),
                    '\\' => out.push_str("\\\\"),
                    other => out.push(*other),
                }
                out.push('\'');
            }
            TokenKind::DocComment(text) => {
                out.push_str("/// ");
                out.push_str(text);
            }
            other => out.push_str(crate::lexer::token_text(other)),
        },
        TokenTree::Group { delim, tokens, .. } => {
            let (open, close) = match delim {
                Delim::Paren => ('(', ')'),
                Delim::Bracket => ('[', ']'),
                Delim::Brace => ('{', '}'),
            };
            out.push(open);
            for token in tokens {
                render_token(token, out);
                out.push(' ');
            }
            out.push(close);
        }
    }
}

fn strip_semis(tokens: Vec<TokenTree>) -> Vec<TokenTree> {
    tokens
        .into_iter()
        .filter(|t| !t.is_kind(&TokenKind::Semi))
        .collect()
}

/// The macros every Teal program gets without importing anything.
pub fn prelude_macros() -> BTreeMap<String, MacroDef> {
    let file = 0;
    let package = "std.prelude";
    let defs = [
        macro_from_source(
            "vec",
            &[("", "[]"), ("$($x:expr),+ $(,)?", "[ $($x),+ ]")],
            file,
            package,
        ),
        macro_from_source(
            "println",
            &[("$($x:expr),* $(,)?", "{ $( let _ = $x; )* unit_value() }")],
            file,
            package,
        ),
        macro_from_source(
            "print",
            &[("$($x:expr),* $(,)?", "{ $( let _ = $x; )* unit_value() }")],
            file,
            package,
        ),
        macro_from_source(
            "eprintln",
            &[("$($x:expr),* $(,)?", "{ $( let _ = $x; )* unit_value() }")],
            file,
            package,
        ),
        macro_from_source(
            "log",
            &[("$($x:expr),* $(,)?", "{ $( let _ = $x; )* unit_value() }")],
            file,
            package,
        ),
        macro_from_source(
            "format",
            &[("$($x:expr),* $(,)?", "{ $( let _ = $x; )* format_impl() }")],
            file,
            package,
        ),
        macro_from_source("panic", &[("$($x:tt)*", "panic_impl()")], file, package),
        macro_from_source(
            "assert",
            &[("$cond:expr", "assert_impl($cond)")],
            file,
            package,
        ),
        macro_from_source(
            "assert_eq",
            &[(
                "$a:expr, $b:expr $(,)?",
                "{ let _ = $a; let _ = $b; unit_value() }",
            )],
            file,
            package,
        ),
        macro_from_source(
            "unreachable",
            &[("$($x:tt)*", "panic_impl()")],
            file,
            package,
        ),
    ];
    defs.into_iter().map(|d| (d.name.clone(), d)).collect()
}

/// Built-in derives are emitted as bodiless impls. Derives are not a closed
/// set: a user-defined `derive_eq` macro takes precedence (DESIGN.md §3.6).
pub fn derive_impl_source(name: &str, generics: &[String], iface: &str) -> String {
    let gen_decl = if generics.is_empty() {
        String::new()
    } else {
        format!("<{}>", generics.join(", "))
    };
    let gen_args = if generics.is_empty() {
        String::new()
    } else {
        format!("<{}>", generics.join(", "))
    };
    match iface {
        "Eq" => format!(
            "impl{gen_decl} Eq for {name}{gen_args} {{ fn eq(self: &Self, other: &Self) -> bool }}"
        ),
        "Hash" => {
            format!("impl{gen_decl} Hash for {name}{gen_args} {{ fn hash(self: &Self) -> u64 }}")
        }
        "Show" => {
            format!("impl{gen_decl} Show for {name}{gen_args} {{ fn show(self: &Self) -> string }}")
        }
        "Clone" => {
            format!("impl{gen_decl} Clone for {name}{gen_args} {{ fn clone(self: &Self) -> Self }}")
        }
        other => format!("impl{gen_decl} {other} for {name}{gen_args} {{ }}"),
    }
}

pub fn derive_tokens(source: &str, span: Span) -> Result<Vec<Token>, Diagnostic> {
    let tokens = lex(source, span.file).map_err(|mut d| {
        d.span = span;
        d
    })?;
    let mut tokens = tokens;
    for tok in &mut tokens {
        tok.span = span;
    }
    Ok(tokens)
}

#[derive(Clone, Debug, Default)]
struct Bindings(BTreeMap<String, Binding>);

#[derive(Clone, Debug)]
enum Binding {
    Tokens(Vec<TokenTree>),
    Reps(Vec<Bindings>),
}

struct Repetition<'a> {
    body: &'a [TokenTree],
    separator: Option<TokenTree>,
    op: char,
    next: usize,
}

fn parse_repetition(pat: &[TokenTree], index: usize) -> Option<Repetition<'_>> {
    if !pat.get(index)?.is_kind(&TokenKind::Dollar) {
        return None;
    }
    let TokenTree::Group { tokens, .. } = pat.get(index + 1)? else {
        return None;
    };
    let mut cursor = index + 2;
    let mut separator = None;
    if !matches!(
        pat.get(cursor).and_then(TokenTree::token).map(|t| &t.kind),
        Some(TokenKind::Star) | Some(TokenKind::Plus) | Some(TokenKind::Question)
    ) {
        separator = pat.get(cursor).cloned();
        cursor += 1;
    }
    let op = match pat.get(cursor).and_then(TokenTree::token).map(|t| &t.kind) {
        Some(TokenKind::Star) => '*',
        Some(TokenKind::Plus) => '+',
        Some(TokenKind::Question) => '?',
        _ => return None,
    };
    Some(Repetition {
        body: tokens,
        separator,
        op,
        next: cursor + 1,
    })
}

fn parse_metavar(pat: &[TokenTree], index: usize) -> Option<(String, String, usize)> {
    if !pat.get(index)?.is_kind(&TokenKind::Dollar) {
        return None;
    }
    let name = match pat.get(index + 1)?.token()?.kind.clone() {
        TokenKind::Ident(name) => name,
        _ => return None,
    };
    if name == "crate" {
        return None;
    }
    if !pat.get(index + 2)?.is_kind(&TokenKind::Colon) {
        return None;
    }
    let frag = match pat.get(index + 3)?.token()?.kind.clone() {
        TokenKind::Ident(frag) => frag,
        _ => return None,
    };
    Some((name, frag, index + 4))
}

fn literal_stop(pat: &[TokenTree], index: usize) -> Option<&TokenTree> {
    match pat.get(index)? {
        TokenTree::Token(_) => pat.get(index),
        TokenTree::Group { .. } => pat.get(index),
    }
}

fn fragment_end(
    input: &[TokenTree],
    start: usize,
    frag: &str,
    stop: Option<&TokenTree>,
) -> Option<usize> {
    let fixed = matches!(frag, "ident" | "literal" | "tt" | "block" | "vis");
    if fixed {
        return match frag {
            "ident" => {
                let tok = input.get(start)?.token()?;
                if matches!(tok.kind, TokenKind::Ident(_)) {
                    Some(start + 1)
                } else {
                    None
                }
            }
            "literal" => {
                let tok = input.get(start)?.token()?;
                if matches!(
                    tok.kind,
                    TokenKind::Integer(_)
                        | TokenKind::Float(_)
                        | TokenKind::Str(_)
                        | TokenKind::Char(_)
                        | TokenKind::True
                        | TokenKind::False
                ) {
                    Some(start + 1)
                } else {
                    None
                }
            }
            "tt" => input.get(start).map(|_| start + 1),
            "block" => match input.get(start)? {
                TokenTree::Group {
                    delim: Delim::Brace,
                    ..
                } => Some(start + 1),
                _ => None,
            },
            "vis" => {
                if input.get(start).is_some_and(|t| t.is_kind(&TokenKind::Pub)) {
                    Some(start + 1)
                } else {
                    Some(start)
                }
            }
            _ => None,
        };
    }

    // Non-fixed fragments consume tokens until the next literal in the
    // matcher, the repetition separator, or the end of the input.
    let mut end = start;
    while end < input.len() {
        if let Some(stop) = stop {
            if same_literal(stop, &input[end]) {
                break;
            }
        }
        end += 1;
    }
    if end == start && frag != "stmt" {
        // Empty fragment is never useful except for an empty statement list.
        if frag == "expr" || frag == "ty" || frag == "path" || frag == "pat" {
            return None;
        }
    }
    Some(end)
}

fn same_literal(pattern: &TokenTree, input: &TokenTree) -> bool {
    match (pattern, input) {
        (TokenTree::Token(a), TokenTree::Token(b)) => a.kind == b.kind,
        (
            TokenTree::Group {
                delim: a,
                tokens: at,
                ..
            },
            TokenTree::Group {
                delim: b,
                tokens: bt,
                ..
            },
        ) => {
            if a != b || at.len() != bt.len() {
                return false;
            }
            at.iter().zip(bt).all(|(x, y)| same_literal(x, y))
        }
        _ => false,
    }
}

fn meta_names(tokens: &[TokenTree], out: &mut BTreeSet<String>) {
    let mut index = 0;
    while index < tokens.len() {
        if let Some((name, _, next)) = parse_metavar(tokens, index) {
            out.insert(name);
            index = next;
            continue;
        }
        if let Some(rep) = parse_repetition(tokens, index) {
            meta_names(rep.body, out);
            index = rep.next;
            continue;
        }
        if let TokenTree::Group { tokens, .. } = &tokens[index] {
            meta_names(tokens, out);
        }
        index += 1;
    }
}

/// Names substituted inside a macro template use `$name` (without a fragment
/// specifier), so they need a separate scanner from matcher metavariables.
fn template_names(tokens: &[TokenTree], out: &mut BTreeSet<String>) {
    let mut index = 0;
    while index < tokens.len() {
        if tokens[index].is_kind(&TokenKind::Dollar) {
            if let Some(token) = tokens.get(index + 1).and_then(TokenTree::token) {
                if let TokenKind::Ident(name) = &token.kind {
                    if name != "crate" {
                        out.insert(name.clone());
                    }
                    index += 2;
                    continue;
                }
            }
        }
        if let Some(rep) = parse_repetition(tokens, index) {
            template_names(rep.body, out);
            index = rep.next;
            continue;
        }
        if let TokenTree::Group { tokens, .. } = &tokens[index] {
            template_names(tokens, out);
        }
        index += 1;
    }
}

fn match_sequence(
    pat: &[TokenTree],
    input: &[TokenTree],
    start: usize,
    binds: &mut Bindings,
) -> Option<usize> {
    match_sequence_with(pat, input, start, binds, None)
}

fn match_sequence_with(
    pat: &[TokenTree],
    input: &[TokenTree],
    start: usize,
    binds: &mut Bindings,
    stop_override: Option<&TokenTree>,
) -> Option<usize> {
    let mut p = 0;
    let mut i = start;
    while p < pat.len() {
        if let Some(rep) = parse_repetition(pat, p) {
            let min = if rep.op == '+' { 1 } else { 0 };
            let max_count = if rep.op == '?' { 1 } else { input.len() + 1 };

            // Collect every successful iteration and the position of its body
            // end. Separators are consumed between iterations; a trailing
            // separator is allowed and treated as an alternative end.
            let mut iterations: Vec<(usize, Bindings)> = Vec::new();
            let mut pos = i;
            while iterations.len() < max_count {
                let mut iter_binds = Bindings::default();
                let Some(end) = match_sequence_with(
                    rep.body,
                    input,
                    pos,
                    &mut iter_binds,
                    rep.separator.as_ref(),
                ) else {
                    break;
                };
                if end == pos {
                    break;
                }
                iterations.push((end, iter_binds));
                pos = end;
                if let Some(separator) = &rep.separator {
                    if input.get(pos).is_some_and(|t| same_literal(separator, t)) {
                        pos += 1;
                    } else {
                        break;
                    }
                } else if end >= input.len() {
                    break;
                }
            }

            let mut names = BTreeSet::new();
            meta_names(rep.body, &mut names);

            for count in (min..=iterations.len()).rev() {
                let mut attempt = binds.clone();
                let mut merged: BTreeMap<String, Vec<Bindings>> = BTreeMap::new();
                for (_, iter) in iterations.iter().take(count) {
                    for name in &names {
                        if iter.0.contains_key(name) {
                            merged.entry(name.clone()).or_default().push(iter.clone());
                        }
                    }
                }
                for (name, reps) in merged {
                    attempt.0.insert(name, Binding::Reps(reps));
                }

                let body_end = if count == 0 {
                    i
                } else {
                    iterations[count - 1].0
                };
                let mut ends = vec![body_end];
                // Allow consuming one trailing separator after the final
                // iteration (the common `$(,)?` form).
                if count == iterations.len() && count > 0 {
                    if let Some(separator) = &rep.separator {
                        if input
                            .get(body_end)
                            .is_some_and(|t| same_literal(separator, t))
                        {
                            ends.push(body_end + 1);
                        }
                    }
                }
                for end in ends {
                    let mut probe = attempt.clone();
                    if let Some(finish) = match_sequence(&pat[rep.next..], input, end, &mut probe) {
                        *binds = probe;
                        return Some(finish);
                    }
                }
            }
            return None;
        }

        if let Some((name, frag, next)) = parse_metavar(pat, p) {
            let stop = literal_stop(pat, next).or(stop_override);
            let end = fragment_end(input, i, &frag, stop)?;
            let tokens = input[i..end].to_vec();
            binds.0.insert(name, Binding::Tokens(tokens));
            i = end;
            p = next;
            continue;
        }

        let pattern = pat.get(p)?;
        let candidate = input.get(i)?;
        match pattern {
            TokenTree::Token(expected) => {
                if candidate.is_kind(&expected.kind) {
                    i += 1;
                    p += 1;
                } else {
                    return None;
                }
            }
            TokenTree::Group {
                delim,
                tokens: inner,
                ..
            } => match candidate {
                TokenTree::Group {
                    delim: got,
                    tokens: got_tokens,
                    ..
                } if got == delim => {
                    if inner.is_empty() {
                        i += 1;
                        p += 1;
                    } else {
                        let mut inner_binds = Bindings::default();
                        let end = match_sequence(inner, got_tokens, 0, &mut inner_binds)?;
                        if end != got_tokens.len() {
                            return None;
                        }
                        for (k, v) in inner_binds.0 {
                            binds.0.insert(k, v);
                        }
                        i += 1;
                        p += 1;
                    }
                }
                _ => return None,
            },
        }
    }
    Some(i)
}

fn match_rule(rule: &MacroRule, input: &[TokenTree]) -> Option<Bindings> {
    let mut binds = Bindings::default();
    let end = match_sequence(&rule.matcher, input, 0, &mut binds)?;
    if end == input.len() {
        Some(binds)
    } else {
        None
    }
}

fn transcribe(
    tokens: &[TokenTree],
    binds: &Bindings,
    binders: &BTreeSet<String>,
    mark: u32,
    package: &str,
    out: &mut Vec<TokenTree>,
    depth: usize,
) -> Result<(), String> {
    if depth > 128 {
        return Err("macro expansion exceeded the recursion limit (128)".into());
    }
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i].is_kind(&TokenKind::Dollar) {
            // $crate
            if tokens.get(i + 1).is_some_and(|t| t.is_ident("crate")) {
                if !package.is_empty() {
                    out.push(TokenTree::Token(Token {
                        kind: TokenKind::Ident(package.to_string()),
                        span: tokens[i].span(),
                    }));
                }
                i += 2;
                continue;
            }

            // Repetition
            if let Some(rep) = parse_repetition(tokens, i) {
                let mut names = BTreeSet::new();
                template_names(rep.body, &mut names);
                let mut count = None;
                for name in &names {
                    let n = match binds.0.get(name) {
                        Some(Binding::Reps(reps)) => reps.len(),
                        Some(Binding::Tokens(_)) => {
                            return Err(format!(
                                "`${name}` is used inside a repetition but is not repeated"
                            ))
                        }
                        None => {
                            return Err(format!("unknown metavariable `${name}` in macro template"))
                        }
                    };
                    match count {
                        Some(existing) if existing != n => {
                            return Err("repetition metavariables have different lengths".into())
                        }
                        _ => count = Some(n),
                    }
                }
                let count = count.unwrap_or(0);
                for k in 0..count {
                    let mut iter = Bindings::default();
                    for name in &names {
                        if let Some(Binding::Reps(reps)) = binds.0.get(name) {
                            if let Some(rep_binds) = reps.get(k) {
                                if let Some(binding) = rep_binds.0.get(name) {
                                    iter.0.insert(name.clone(), binding.clone());
                                }
                            }
                        }
                    }
                    transcribe(rep.body, &iter, binders, mark, package, out, depth + 1)?;
                    if k + 1 < count {
                        if let Some(separator) = &rep.separator {
                            out.push(separator.clone());
                        }
                    }
                }
                i = rep.next;
                continue;
            }

            // Metavariable substitution
            if let Some(token) = tokens.get(i + 1).and_then(TokenTree::token) {
                if let TokenKind::Ident(name) = &token.kind {
                    match binds.0.get(name) {
                        Some(Binding::Tokens(replacement)) => {
                            out.extend(replacement.iter().cloned());
                            i += 2;
                            continue;
                        }
                        Some(Binding::Reps(_)) => {
                            return Err(format!(
                                "`${name}` is repeated but used outside a repetition"
                            ))
                        }
                        None => return Err(format!("unknown metavariable `${name}`")),
                    }
                }
            }
            return Err("expected metavariable after `$`".into());
        }

        match &tokens[i] {
            TokenTree::Token(tok) => {
                if let TokenKind::Ident(name) = &tok.kind {
                    if binders.contains(name) {
                        out.push(TokenTree::Token(Token {
                            kind: TokenKind::Ident(format!("{name}#{mark}")),
                            span: tok.span,
                        }));
                        i += 1;
                        continue;
                    }
                }
                out.push(TokenTree::Token(tok.clone()));
            }
            TokenTree::Group {
                delim,
                tokens: inner,
                span,
            } => {
                let mut nested = Vec::new();
                transcribe(inner, binds, binders, mark, package, &mut nested, depth + 1)?;
                out.push(TokenTree::Group {
                    delim: *delim,
                    tokens: nested,
                    span: *span,
                });
            }
        }
        i += 1;
    }
    Ok(())
}

/// Template-introduced binders: `let (mut)? x`, `for x`, and closure parameters
/// delimited by `| ... |`. Metavariables are never binders.
fn collect_binders(tokens: &[TokenTree]) -> BTreeSet<String> {
    let mut binders = BTreeSet::new();
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i].is_kind(&TokenKind::Let) {
            let mut j = i + 1;
            if tokens.get(j).is_some_and(|t| t.is_kind(&TokenKind::Mut)) {
                j += 1;
            }
            if let Some(name) = identifier_at(tokens, j) {
                binders.insert(name);
            }
        } else if tokens[i].is_kind(&TokenKind::For) {
            if let Some(name) = identifier_at(tokens, i + 1) {
                binders.insert(name);
            }
        } else if tokens[i].is_kind(&TokenKind::Pipe) {
            // Closure parameter list: |a: T, b| ...
            let mut j = i + 1;
            while j < tokens.len() && !tokens[j].is_kind(&TokenKind::Pipe) {
                if let Some(name) = identifier_at(tokens, j) {
                    binders.insert(name);
                }
                j += 1;
            }
        }
        if let TokenTree::Group { tokens, .. } = &tokens[i] {
            binders.extend(collect_binders(tokens));
        }
        i += 1;
    }
    binders
}

fn identifier_at(tokens: &[TokenTree], index: usize) -> Option<String> {
    if index > 0 && tokens[index - 1].is_kind(&TokenKind::Dollar) {
        return None;
    }
    match &tokens.get(index)?.token()?.kind {
        TokenKind::Ident(name) if name != "mut" => Some(name.clone()),
        _ => None,
    }
}

pub struct Expander<'a> {
    pub defs: &'a BTreeMap<String, MacroDef>,
    pub depth: usize,
    pub invocations: u64,
}

impl<'a> Expander<'a> {
    pub fn new(defs: &'a BTreeMap<String, MacroDef>) -> Self {
        Self {
            defs,
            depth: 0,
            invocations: 0,
        }
    }

    pub fn definition(&self, name: &str) -> Option<&MacroDef> {
        self.defs.get(name)
    }

    pub fn expand(
        &mut self,
        name: &str,
        input: &[TokenTree],
        span: Span,
        package: &str,
    ) -> Result<Vec<TokenTree>, Diagnostic> {
        if self.depth >= 128 {
            return Err(Diagnostic::error(
                codes::MACRO,
                span,
                "macro expansion exceeded the recursion limit (128)",
            ));
        }
        let Some(def) = self.defs.get(name) else {
            return Err(
                Diagnostic::error(codes::MACRO, span, format!("unknown macro `{name}!`"))
                    .with_note("declare it with `macro name! { ... }` before use"),
            );
        };

        let mut matched = None;
        for rule in &def.rules {
            if let Some(binds) = match_rule(rule, input) {
                matched = Some((rule, binds));
                break;
            }
        }
        let Some((rule, binds)) = matched else {
            return Err(Diagnostic::error(
                codes::MACRO,
                span,
                format!("no rule of `{name}!` matches this invocation"),
            ));
        };

        self.invocations += 1;
        let mark = (u32::try_from(self.invocations).unwrap_or(u32::MAX))
            .wrapping_mul(2654435761)
            .wrapping_add(span.start)
            .wrapping_add(span.end)
            .wrapping_add((span.file << 16) as u32);

        let binders = collect_binders(&rule.transcriber);
        let mut out = Vec::new();
        self.depth += 1;
        let result = transcribe(
            &rule.transcriber,
            &binds,
            &binders,
            mark,
            package,
            &mut out,
            self.depth,
        );
        self.depth -= 1;
        result.map_err(|message| Diagnostic::error(codes::MACRO, span, message))?;

        // 1 MiB expansion budget per invocation (DESIGN.md §3.6).
        let size: usize = out.iter().map(tree_size).sum();
        if size > 1024 * 1024 {
            return Err(Diagnostic::error(
                codes::MACRO,
                span,
                "macro expansion exceeded the 1 MiB budget",
            ));
        }
        Ok(out)
    }
}

fn tree_size(tree: &TokenTree) -> usize {
    match tree {
        TokenTree::Token(_) => 1,
        TokenTree::Group { tokens, .. } => 1 + tokens.iter().map(tree_size).sum::<usize>(),
    }
}

/// Canonical canonicalization of a rule table, used for `.tli` digests.
pub fn macro_digest(def: &MacroDef) -> String {
    fn render(tokens: &[TokenTree], out: &mut String) {
        for token in tokens {
            match token {
                TokenTree::Token(tok) => {
                    out.push_str(&format!("{:?};", tok.kind));
                }
                TokenTree::Group { delim, tokens, .. } => {
                    out.push_str(&format!("{:?}{{", delim));
                    render(tokens, out);
                    out.push('}');
                }
            }
        }
    }
    let mut text = format!("{}|", def.name);
    for rule in &def.rules {
        render(&rule.matcher, &mut text);
        text.push_str("=>");
        render(&rule.transcriber, &mut text);
        text.push('|');
    }
    crate::hash::to_hex(&crate::hash::hash_str(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expand(name: &str, args: &str) -> String {
        let defs = prelude_macros();
        let mut expander = Expander::new(&defs);
        let tokens = lex(&format!("({args})"), 0).unwrap();
        let trees = tree_stream(&tokens).unwrap();
        let input = match &trees[0] {
            TokenTree::Group { tokens, .. } => tokens.clone(),
            _ => panic!(),
        };
        let out = expander
            .expand(name, &input, Span::dummy(), "demo")
            .unwrap();
        format!("{:?}", out)
    }

    #[test]
    fn expands_vec_rule() {
        let out = expand("vec", "1, 2, 3");
        assert!(out.contains("1"));
        assert!(out.contains("3"), "{out}");
    }

    #[test]
    fn expands_empty_vec_rule() {
        let out = expand("vec", "");
        assert!(out.contains("Bracket"));
    }

    #[test]
    fn hygienic_let_gets_mark() {
        let def = macro_from_source(
            "shadow",
            &[("$x:expr", "{ let __tmp = $x; __tmp }")],
            0,
            "demo",
        );
        let mut defs = BTreeMap::new();
        defs.insert(def.name.clone(), def);
        let mut expander = Expander::new(&defs);
        let tokens = lex("(1)", 0).unwrap();
        let trees = tree_stream(&tokens).unwrap();
        let input = match &trees[0] {
            TokenTree::Group { tokens, .. } => tokens.clone(),
            _ => panic!(),
        };
        let out = expander
            .expand("shadow", &input, Span::dummy(), "demo")
            .unwrap();
        let text = format!("{:?}", out);
        assert!(text.contains("__tmp#"), "{text}");
    }

    #[test]
    fn rejects_unknown_macro() {
        let defs = prelude_macros();
        let mut expander = Expander::new(&defs);
        assert!(expander.expand("nope", &[], Span::dummy(), "demo").is_err());
    }
}
