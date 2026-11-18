//! Hand-written LL(1) lexer.
//!
//! In addition to ordinary tokenization it performs Teal's newline statement
//! termination (DESIGN.md §3.8): a newline becomes `;` when the previous token
//! is an identifier, literal, `)`, `]`, `}`, `?`, or a diverging keyword. A
//! newline directly before `}` or `else` never terminates a statement, so
//! block-tail expressions and `} else {` chains read naturally.

use crate::{codes, Diagnostic, FileId, Span};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum TokenKind {
    Ident(String),
    Integer(String),
    Float(String),
    Str(String),
    Char(char),
    DocComment(String),

    True,
    False,

    Semi,
    Colon,
    ColonColon,
    Comma,
    Dot,
    DotDot,
    DotDotEq,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Pipe,
    PipePipe,
    Amp,
    AmpAmp,
    Caret,
    Question,
    At,
    Arrow,
    FatArrow,
    Eq,
    EqEq,
    Bang,
    BangEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    Shl,
    Shr,
    Plus,
    PlusEq,
    Minus,
    MinusEq,
    Star,
    StarEq,
    Slash,
    SlashEq,
    Percent,
    PercentEq,
    Dollar,
    Pound,

    Package,
    Import,
    As,
    Pub,
    Const,
    Struct,
    Enum,
    Open,
    Opaque,
    Type,
    Interface,
    Impl,
    For,
    Fn,
    Macro,
    Match,
    Let,
    Mut,
    If,
    Else,
    While,
    Loop,
    Return,
    Break,
    Continue,
    Defer,
    Try,
    In,
    SelfValue,
    Dyn,
    Where,

    Eof,
    #[default]
    Unknown,
}

impl std::fmt::Display for TokenKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenKind::Ident(name) => write!(f, "`{name}`"),
            TokenKind::Integer(text) | TokenKind::Float(text) => write!(f, "`{text}`"),
            TokenKind::Str(text) => write!(f, "string literal `{:?}`", text),
            TokenKind::Char(c) => write!(f, "character literal `{:?}`", c),
            TokenKind::DocComment(_) => write!(f, "doc comment"),
            other => write!(f, "`{}`", token_text(other)),
        }
    }
}

/// Canonical spelling of punctuation/keywords, used in messages and macro
/// matching.
pub fn token_text(kind: &TokenKind) -> &'static str {
    match kind {
        TokenKind::True => "true",
        TokenKind::False => "false",
        TokenKind::Semi => ";",
        TokenKind::Colon => ":",
        TokenKind::ColonColon => "::",
        TokenKind::Comma => ",",
        TokenKind::Dot => ".",
        TokenKind::DotDot => "..",
        TokenKind::DotDotEq => "..=",
        TokenKind::LParen => "(",
        TokenKind::RParen => ")",
        TokenKind::LBracket => "[",
        TokenKind::RBracket => "]",
        TokenKind::LBrace => "{",
        TokenKind::RBrace => "}",
        TokenKind::Pipe => "|",
        TokenKind::PipePipe => "||",
        TokenKind::Amp => "&",
        TokenKind::AmpAmp => "&&",
        TokenKind::Caret => "^",
        TokenKind::Question => "?",
        TokenKind::At => "@",
        TokenKind::Arrow => "->",
        TokenKind::FatArrow => "=>",
        TokenKind::Eq => "=",
        TokenKind::EqEq => "==",
        TokenKind::Bang => "!",
        TokenKind::BangEq => "!=",
        TokenKind::Lt => "<",
        TokenKind::LtEq => "<=",
        TokenKind::Gt => ">",
        TokenKind::GtEq => ">=",
        TokenKind::Shl => "<<",
        TokenKind::Shr => ">>",
        TokenKind::Plus => "+",
        TokenKind::PlusEq => "+=",
        TokenKind::Minus => "-",
        TokenKind::MinusEq => "-=",
        TokenKind::Star => "*",
        TokenKind::StarEq => "*=",
        TokenKind::Slash => "/",
        TokenKind::SlashEq => "/=",
        TokenKind::Percent => "%",
        TokenKind::PercentEq => "%=",
        TokenKind::Dollar => "$",
        TokenKind::Pound => "#",
        TokenKind::Package => "package",
        TokenKind::Import => "import",
        TokenKind::As => "as",
        TokenKind::Pub => "pub",
        TokenKind::Const => "const",
        TokenKind::Struct => "struct",
        TokenKind::Enum => "enum",
        TokenKind::Open => "open",
        TokenKind::Opaque => "opaque",
        TokenKind::Type => "type",
        TokenKind::Interface => "interface",
        TokenKind::Impl => "impl",
        TokenKind::For => "for",
        TokenKind::Fn => "fn",
        TokenKind::Macro => "macro",
        TokenKind::Match => "match",
        TokenKind::Let => "let",
        TokenKind::Mut => "mut",
        TokenKind::If => "if",
        TokenKind::Else => "else",
        TokenKind::While => "while",
        TokenKind::Loop => "loop",
        TokenKind::Return => "return",
        TokenKind::Break => "break",
        TokenKind::Continue => "continue",
        TokenKind::Defer => "defer",
        TokenKind::Try => "try",
        TokenKind::In => "in",
        TokenKind::SelfValue => "self",
        TokenKind::Dyn => "dyn",
        TokenKind::Where => "where",
        TokenKind::Eof => "<eof>",
        TokenKind::Ident(_)
        | TokenKind::Integer(_)
        | TokenKind::Float(_)
        | TokenKind::Str(_)
        | TokenKind::Char(_)
        | TokenKind::DocComment(_)
        | TokenKind::Unknown => "?",
    }
}

fn keyword(word: &str) -> Option<TokenKind> {
    Some(match word {
        "package" => TokenKind::Package,
        "import" => TokenKind::Import,
        "as" => TokenKind::As,
        "pub" => TokenKind::Pub,
        "const" => TokenKind::Const,
        "struct" => TokenKind::Struct,
        "enum" => TokenKind::Enum,
        "open" => TokenKind::Open,
        "opaque" => TokenKind::Opaque,
        "type" => TokenKind::Type,
        "interface" => TokenKind::Interface,
        "impl" => TokenKind::Impl,
        "for" => TokenKind::For,
        "fn" => TokenKind::Fn,
        "macro" => TokenKind::Macro,
        "match" => TokenKind::Match,
        "let" => TokenKind::Let,
        "mut" => TokenKind::Mut,
        "if" => TokenKind::If,
        "else" => TokenKind::Else,
        "while" => TokenKind::While,
        "loop" => TokenKind::Loop,
        "return" => TokenKind::Return,
        "break" => TokenKind::Break,
        "continue" => TokenKind::Continue,
        "defer" => TokenKind::Defer,
        "try" => TokenKind::Try,
        "in" => TokenKind::In,
        "self" => TokenKind::SelfValue,
        "dyn" => TokenKind::Dyn,
        "where" => TokenKind::Where,
        "true" => TokenKind::True,
        "false" => TokenKind::False,
        _ => return None,
    })
}

pub fn lex(source: &str, file: FileId) -> Result<Vec<Token>, Diagnostic> {
    Lexer::new(source, file).run()
}

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    file: FileId,
    pos: usize,
    tokens: Vec<Token>,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str, file: FileId) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            file,
            pos: 0,
            tokens: Vec::new(),
        }
    }

    fn error(&self, start: usize, end: usize, message: impl Into<String>) -> Diagnostic {
        Diagnostic::error(codes::LEX, Span::new(self.file, start, end), message)
    }

    fn prev_terminates(&self) -> bool {
        matches!(
            self.tokens.last().map(|t| &t.kind),
            Some(
                TokenKind::Ident(_)
                    | TokenKind::Integer(_)
                    | TokenKind::Float(_)
                    | TokenKind::Str(_)
                    | TokenKind::Char(_)
                    | TokenKind::True
                    | TokenKind::False
                    | TokenKind::SelfValue
                    | TokenKind::Return
                    | TokenKind::Break
                    | TokenKind::Continue
                    | TokenKind::RParen
                    | TokenKind::RBracket
                    | TokenKind::RBrace
                    | TokenKind::Question
            )
        )
    }

    /// Look past whitespace/comments to decide whether a newline should become
    /// a statement terminator. Returns true if the next significant character
    /// starts `}` or the `else` keyword.
    fn newline_is_transparent(&self, from: usize) -> bool {
        let mut i = from;
        while i < self.bytes.len() {
            match self.bytes[i] {
                b' ' | b'\t' | b'\r' | b'\n' => i += 1,
                b'/' if self.bytes.get(i + 1) == Some(&b'/') => {
                    while i < self.bytes.len() && self.bytes[i] != b'\n' {
                        i += 1;
                    }
                }
                b'/' if self.bytes.get(i + 1) == Some(&b'*') => {
                    i += 2;
                    while i + 1 < self.bytes.len()
                        && !(self.bytes[i] == b'*' && self.bytes[i + 1] == b'/')
                    {
                        i += 1;
                    }
                    i = (i + 2).min(self.bytes.len());
                }
                b'}' => return true,
                b'e' => {
                    let rest = &self.src[i..];
                    if rest.starts_with("else")
                        && !rest[4..]
                            .chars()
                            .next()
                            .is_some_and(|c| c.is_alphanumeric() || c == '_')
                    {
                        return true;
                    }
                    return false;
                }
                _ => return false,
            }
        }
        false
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        self.tokens.push(Token {
            kind,
            span: Span::new(self.file, start, self.pos),
        });
    }

    fn run(mut self) -> Result<Vec<Token>, Diagnostic> {
        while self.pos < self.bytes.len() {
            let c = self.bytes[self.pos];
            match c {
                b' ' | b'\t' | b'\r' => {
                    self.pos += 1;
                }
                b'\n' => {
                    let at = self.pos;
                    self.pos += 1;
                    if self.prev_terminates() && !self.newline_is_transparent(self.pos) {
                        self.push(TokenKind::Semi, at);
                    }
                }
                b'/' if self.bytes.get(self.pos + 1) == Some(&b'/') => {
                    let start = self.pos;
                    let is_doc = self.bytes.get(self.pos + 2) == Some(&b'/')
                        && self.bytes.get(self.pos + 3) != Some(&b'/');
                    let is_inner = self.bytes.get(self.pos + 2) == Some(&b'!');
                    self.pos += 2;
                    if is_doc || is_inner {
                        self.pos += 1;
                    }
                    let content_start = self.pos;
                    while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
                        self.pos += 1;
                    }
                    if is_doc || is_inner {
                        let text = self.src[content_start..self.pos].trim().to_string();
                        self.push(TokenKind::DocComment(text), start);
                    }
                }
                b'/' if self.bytes.get(self.pos + 1) == Some(&b'*') => {
                    let start = self.pos;
                    self.pos += 2;
                    while self.pos + 1 < self.bytes.len()
                        && !(self.bytes[self.pos] == b'*' && self.bytes[self.pos + 1] == b'/')
                    {
                        self.pos += 1;
                    }
                    if self.pos + 1 >= self.bytes.len() {
                        return Err(self.error(
                            start,
                            self.bytes.len(),
                            "unterminated block comment",
                        ));
                    }
                    self.pos += 2;
                }
                _ => {
                    let start = self.pos;
                    let kind = self.next_token()?;
                    self.push(kind, start);
                }
            }
        }
        self.tokens.push(Token {
            kind: TokenKind::Eof,
            span: Span::new(self.file, self.pos, self.pos),
        });
        Ok(self.tokens)
    }

    fn next_token(&mut self) -> Result<TokenKind, Diagnostic> {
        let start = self.pos;
        let c = self.bytes[self.pos];
        if c == b'"' {
            return self.string_literal();
        }
        if c == b'\'' {
            return self.char_literal();
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            self.pos += 1;
            while self.pos < self.bytes.len()
                && (self.bytes[self.pos].is_ascii_alphanumeric() || self.bytes[self.pos] == b'_')
            {
                self.pos += 1;
            }
            let word = &self.src[start..self.pos];
            return Ok(keyword(word).unwrap_or_else(|| TokenKind::Ident(word.to_string())));
        }
        if c.is_ascii_digit() {
            return Ok(self.number_literal());
        }

        macro_rules! punct {
            ($len:expr, $kind:expr) => {{
                self.pos += $len;
                return Ok($kind);
            }};
        }

        match c {
            b'.' => {
                if self.bytes.get(self.pos + 1) == Some(&b'.') {
                    if self.bytes.get(self.pos + 2) == Some(&b'=') {
                        punct!(3, TokenKind::DotDotEq)
                    }
                    punct!(2, TokenKind::DotDot)
                } else {
                    punct!(1, TokenKind::Dot)
                }
            }
            b'(' => punct!(1, TokenKind::LParen),
            b')' => punct!(1, TokenKind::RParen),
            b'[' => punct!(1, TokenKind::LBracket),
            b']' => punct!(1, TokenKind::RBracket),
            b'{' => punct!(1, TokenKind::LBrace),
            b'}' => punct!(1, TokenKind::RBrace),
            b',' => punct!(1, TokenKind::Comma),
            b';' => punct!(1, TokenKind::Semi),
            b'@' => punct!(1, TokenKind::At),
            b'~' => return Err(self.error(start, start + 1, "`~` is not a Teal operator")),
            b'?' => punct!(1, TokenKind::Question),
            b'$' => punct!(1, TokenKind::Dollar),
            b'#' => punct!(1, TokenKind::Pound),
            b':' => {
                if self.bytes.get(self.pos + 1) == Some(&b':') {
                    punct!(2, TokenKind::ColonColon)
                }
                punct!(1, TokenKind::Colon)
            }
            b'-' => {
                if self.bytes.get(self.pos + 1) == Some(&b'>') {
                    punct!(2, TokenKind::Arrow)
                }
                if self.bytes.get(self.pos + 1) == Some(&b'=') {
                    punct!(2, TokenKind::MinusEq)
                }
                punct!(1, TokenKind::Minus)
            }
            b'=' => {
                if self.bytes.get(self.pos + 1) == Some(&b'>') {
                    punct!(2, TokenKind::FatArrow)
                }
                if self.bytes.get(self.pos + 1) == Some(&b'=') {
                    punct!(2, TokenKind::EqEq)
                }
                punct!(1, TokenKind::Eq)
            }
            b'!' => {
                if self.bytes.get(self.pos + 1) == Some(&b'=') {
                    punct!(2, TokenKind::BangEq)
                }
                punct!(1, TokenKind::Bang)
            }
            b'<' => {
                if self.bytes.get(self.pos + 1) == Some(&b'=') {
                    punct!(2, TokenKind::LtEq)
                }
                if self.bytes.get(self.pos + 1) == Some(&b'<') {
                    if self.bytes.get(self.pos + 2) == Some(&b'=') {
                        self.pos += 3;
                        return Ok(TokenKind::Lt);
                    }
                    punct!(2, TokenKind::Shl)
                }
                punct!(1, TokenKind::Lt)
            }
            b'>' => {
                if self.bytes.get(self.pos + 1) == Some(&b'=') {
                    punct!(2, TokenKind::GtEq)
                }
                if self.bytes.get(self.pos + 1) == Some(&b'>') {
                    if self.bytes.get(self.pos + 2) == Some(&b'=') {
                        self.pos += 3;
                        return Ok(TokenKind::Gt);
                    }
                    punct!(2, TokenKind::Shr)
                }
                punct!(1, TokenKind::Gt)
            }
            b'+' => {
                if self.bytes.get(self.pos + 1) == Some(&b'=') {
                    punct!(2, TokenKind::PlusEq)
                }
                punct!(1, TokenKind::Plus)
            }
            b'*' => {
                if self.bytes.get(self.pos + 1) == Some(&b'=') {
                    punct!(2, TokenKind::StarEq)
                }
                punct!(1, TokenKind::Star)
            }
            b'/' => {
                if self.bytes.get(self.pos + 1) == Some(&b'=') {
                    punct!(2, TokenKind::SlashEq)
                }
                punct!(1, TokenKind::Slash)
            }
            b'%' => {
                if self.bytes.get(self.pos + 1) == Some(&b'=') {
                    punct!(2, TokenKind::PercentEq)
                }
                punct!(1, TokenKind::Percent)
            }
            b'&' => {
                if self.bytes.get(self.pos + 1) == Some(&b'&') {
                    punct!(2, TokenKind::AmpAmp)
                }
                punct!(1, TokenKind::Amp)
            }
            b'|' => {
                if self.bytes.get(self.pos + 1) == Some(&b'|') {
                    punct!(2, TokenKind::PipePipe)
                }
                punct!(1, TokenKind::Pipe)
            }
            b'^' => punct!(1, TokenKind::Caret),
            b'\\' => return Err(self.error(start, start + 1, "unexpected `\\` outside a string")),
            _ => Err(self.error(
                start,
                start + 1,
                format!(
                    "unexpected character `{}`",
                    self.src[start..].chars().next().unwrap()
                ),
            )),
        }
    }

    fn number_literal(&mut self) -> TokenKind {
        let start = self.pos;
        let mut is_float = false;
        if self.bytes[self.pos] == b'0'
            && matches!(
                self.bytes.get(self.pos + 1),
                Some(b'x') | Some(b'X') | Some(b'b') | Some(b'B') | Some(b'o') | Some(b'O')
            )
        {
            self.pos += 2;
            while self.pos < self.bytes.len()
                && (self.bytes[self.pos].is_ascii_alphanumeric() || self.bytes[self.pos] == b'_')
            {
                self.pos += 1;
            }
            return TokenKind::Integer(self.src[start..self.pos].to_string());
        }
        while self.pos < self.bytes.len()
            && (self.bytes[self.pos].is_ascii_digit() || self.bytes[self.pos] == b'_')
        {
            self.pos += 1;
        }
        if self.bytes.get(self.pos) == Some(&b'.')
            && self
                .bytes
                .get(self.pos + 1)
                .is_some_and(|b| b.is_ascii_digit())
        {
            is_float = true;
            self.pos += 1;
            while self.pos < self.bytes.len()
                && (self.bytes[self.pos].is_ascii_digit() || self.bytes[self.pos] == b'_')
            {
                self.pos += 1;
            }
        }
        if matches!(self.bytes.get(self.pos), Some(b'e') | Some(b'E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.bytes.get(self.pos), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }
        // Numeric type suffix (`1usize`, `1.5f32`).
        if self
            .bytes
            .get(self.pos)
            .is_some_and(|b| b.is_ascii_alphabetic())
        {
            while self.pos < self.bytes.len()
                && (self.bytes[self.pos].is_ascii_alphanumeric() || self.bytes[self.pos] == b'_')
            {
                self.pos += 1;
            }
        }
        let text = self.src[start..self.pos].to_string();
        if is_float {
            TokenKind::Float(text)
        } else {
            TokenKind::Integer(text)
        }
    }

    fn string_literal(&mut self) -> Result<TokenKind, Diagnostic> {
        let start = self.pos;
        self.pos += 1;
        let mut value = String::new();
        loop {
            if self.pos >= self.bytes.len() {
                return Err(self.error(start, self.pos, "unterminated string literal"));
            }
            match self.bytes[self.pos] {
                b'"' => {
                    self.pos += 1;
                    return Ok(TokenKind::Str(value));
                }
                b'\n' => {
                    return Err(self.error(start, self.pos, "unterminated string literal"));
                }
                b'\\' => {
                    self.pos += 1;
                    value.push(self.escape(start)?);
                }
                _ => {
                    let ch = self.src[self.pos..].chars().next().unwrap();
                    self.pos += ch.len_utf8();
                    value.push(ch);
                }
            }
        }
    }

    fn char_literal(&mut self) -> Result<TokenKind, Diagnostic> {
        let start = self.pos;
        self.pos += 1;
        if self.pos >= self.bytes.len() {
            return Err(self.error(start, self.pos, "unterminated character literal"));
        }
        let ch = if self.bytes[self.pos] == b'\\' {
            self.pos += 1;
            self.escape(start)?
        } else {
            let ch = self.src[self.pos..].chars().next().unwrap();
            self.pos += ch.len_utf8();
            ch
        };
        if self.bytes.get(self.pos) != Some(&b'\'') {
            return Err(self.error(start, self.pos, "unterminated character literal"));
        }
        self.pos += 1;
        Ok(TokenKind::Char(ch))
    }

    fn escape(&mut self, start: usize) -> Result<char, Diagnostic> {
        if self.pos >= self.bytes.len() {
            return Err(self.error(start, self.pos, "unterminated escape sequence"));
        }
        let c = self.bytes[self.pos];
        self.pos += 1;
        Ok(match c {
            b'n' => '\n',
            b't' => '\t',
            b'r' => '\r',
            b'0' => '\0',
            b'\\' => '\\',
            b'\'' => '\'',
            b'"' => '"',
            b'x' => {
                let hex = self.src.get(self.pos..self.pos + 2).ok_or_else(|| {
                    self.error(start, self.pos, "expected two hex digits after `\\x`")
                })?;
                let value = u8::from_str_radix(hex, 16)
                    .map_err(|_| self.error(start, self.pos + 2, "invalid hex escape"))?;
                self.pos += 2;
                value as char
            }
            b'u' => {
                if self.bytes.get(self.pos) != Some(&b'{') {
                    return Err(self.error(start, self.pos, "expected `{` after `\\u`"));
                }
                self.pos += 1;
                let digits_start = self.pos;
                while self.pos < self.bytes.len() && self.bytes[self.pos] != b'}' {
                    self.pos += 1;
                }
                let digits = &self.src[digits_start..self.pos];
                self.pos = (self.pos + 1).min(self.bytes.len());
                let value = u32::from_str_radix(digits, 16)
                    .map_err(|_| self.error(start, self.pos, "invalid unicode escape"))?;
                char::from_u32(value)
                    .ok_or_else(|| self.error(start, self.pos, "invalid unicode scalar value"))?
            }
            other => {
                return Err(self.error(
                    start,
                    self.pos,
                    format!("unknown escape sequence `\\{}`", other as char),
                ))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<TokenKind> {
        lex(source, 0)
            .unwrap()
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn inserts_semis_at_newlines() {
        let ks = kinds("let x = 1\nlet y = 2");
        assert_eq!(ks.iter().filter(|k| **k == TokenKind::Semi).count(), 1);
        let ks = kinds("let x = 1\nlet y = 2\n");
        assert_eq!(ks.iter().filter(|k| **k == TokenKind::Semi).count(), 2);
    }

    #[test]
    fn does_not_terminate_before_closing_brace() {
        let ks = kinds("{\n  value\n}");
        assert!(!ks.contains(&TokenKind::Semi));
    }

    #[test]
    fn does_not_terminate_before_else() {
        let ks = kinds("if x {\n}\nelse {\n}");
        assert_eq!(ks.iter().filter(|k| **k == TokenKind::Semi).count(), 0);
    }

    #[test]
    fn lexes_operators_and_literals() {
        let ks = kinds("a << 2 >= 0x10 && true");
        assert!(ks.contains(&TokenKind::Shl));
        assert!(ks.contains(&TokenKind::GtEq));
        assert!(ks.contains(&TokenKind::AmpAmp));
        assert!(ks.contains(&TokenKind::Integer("0x10".into())));
    }

    #[test]
    fn lexes_doc_comments() {
        let ks = kinds("/// hello\nstruct S {}\n");
        assert!(matches!(ks[0], TokenKind::DocComment(ref s) if s == "hello"));
    }

    #[test]
    fn range_does_not_become_float() {
        let ks = kinds("1..=3");
        assert_eq!(
            ks,
            vec![
                TokenKind::Integer("1".into()),
                TokenKind::DotDotEq,
                TokenKind::Integer("3".into()),
                TokenKind::Eof
            ]
        );
    }
}
