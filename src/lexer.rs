use crate::{Diagnostic, Span};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    Ident(String),
    Integer(String),
    Float(String),
    String(String),
    Char(char),
    True,
    False,
    Semi,
    Colon,
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
    Amp,
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
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    ColonColon,
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
    SelfVal,
    Dyn,
    Where,
    Eof,
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

pub fn lex(source: &str) -> Result<Vec<Token>, Diagnostic> {
    let mut out = Vec::new();
    let bytes = source.as_bytes();
    let mut i = 0;
    let mut previous_terminates = false;

    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_whitespace() {
            if c == '\n' && previous_terminates {
                out.push(Token {
                    kind: TokenKind::Semi,
                    span: Span::new(i, i + 1),
                });
                previous_terminates = false;
            }
            i += 1;
            continue;
        }
        if c == '/' && bytes.get(i + 1).copied() == Some(b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let start = i;
        let kind = match c {
            '"' => {
                i += 1;
                let value_start = i;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                if i >= bytes.len() {
                    return Err(Diagnostic {
                        span: Span::new(start, i),
                        message: "unterminated string".into(),
                    });
                }
                i += 1;
                TokenKind::String(source[value_start..i - 1].to_string())
            }
            '\'' => {
                i += 1;
                let ch = bytes[i] as char;
                i += 1;
                if i >= bytes.len() || bytes[i] != b'\'' {
                    return Err(Diagnostic {
                        span: Span::new(start, i),
                        message: "unterminated character".into(),
                    });
                }
                i += 1;
                TokenKind::Char(ch)
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                i += 1;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let word = &source[start..i];
                match word {
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
                    "self" => TokenKind::SelfVal,
                    "dyn" => TokenKind::Dyn,
                    "where" => TokenKind::Where,
                    "true" => TokenKind::True,
                    "false" => TokenKind::False,
                    _ => TokenKind::Ident(word.to_string()),
                }
            }
            c if c.is_ascii_digit() => {
                i += 1;
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
                    i += 1;
                }
                let kind = if i < bytes.len() && bytes[i] == b'.' {
                    i += 1;
                    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
                        i += 1;
                    }
                    TokenKind::Float(source[start..i].to_string())
                } else {
                    TokenKind::Integer(source[start..i].to_string())
                };
                kind
            }
            ':' if bytes.get(i + 1).copied() == Some(b':') => {
                i += 2;
                TokenKind::ColonColon
            }
            '.' if bytes.get(i + 1).copied() == Some(b'.')
                && bytes.get(i + 2).copied() == Some(b'=') =>
            {
                i += 3;
                TokenKind::DotDotEq
            }
            '.' if bytes.get(i + 1).copied() == Some(b'.') => {
                i += 2;
                TokenKind::DotDot
            }
            '-' if bytes.get(i + 1).copied() == Some(b'>') => {
                i += 2;
                TokenKind::Arrow
            }
            '=' if bytes.get(i + 1).copied() == Some(b'>') => {
                i += 2;
                TokenKind::FatArrow
            }
            '=' if bytes.get(i + 1).copied() == Some(b'=') => {
                i += 2;
                TokenKind::EqEq
            }
            '!' if bytes.get(i + 1).copied() == Some(b'=') => {
                i += 2;
                TokenKind::BangEq
            }
            '<' if bytes.get(i + 1).copied() == Some(b'=') => {
                i += 2;
                TokenKind::LtEq
            }
            '>' if bytes.get(i + 1).copied() == Some(b'=') => {
                i += 2;
                TokenKind::GtEq
            }
            ':' => {
                i += 1;
                TokenKind::Colon
            }
            ',' => {
                i += 1;
                TokenKind::Comma
            }
            '.' => {
                i += 1;
                TokenKind::Dot
            }
            '(' => {
                i += 1;
                TokenKind::LParen
            }
            ')' => {
                i += 1;
                TokenKind::RParen
            }
            '[' => {
                i += 1;
                TokenKind::LBracket
            }
            ']' => {
                i += 1;
                TokenKind::RBracket
            }
            '{' => {
                i += 1;
                TokenKind::LBrace
            }
            '}' => {
                i += 1;
                TokenKind::RBrace
            }
            '|' => {
                i += 1;
                TokenKind::Pipe
            }
            '&' => {
                i += 1;
                TokenKind::Amp
            }
            '?' => {
                i += 1;
                TokenKind::Question
            }
            '@' => {
                i += 1;
                TokenKind::At
            }
            '=' => {
                i += 1;
                TokenKind::Eq
            }
            '!' => {
                i += 1;
                TokenKind::Bang
            }
            '<' => {
                i += 1;
                TokenKind::Lt
            }
            '>' => {
                i += 1;
                TokenKind::Gt
            }
            '+' => {
                i += 1;
                TokenKind::Plus
            }
            '-' => {
                i += 1;
                TokenKind::Minus
            }
            '*' => {
                i += 1;
                TokenKind::Star
            }
            '/' => {
                i += 1;
                TokenKind::Slash
            }
            '%' => {
                i += 1;
                TokenKind::Percent
            }
            ';' => {
                i += 1;
                TokenKind::Semi
            }
            '$' => {
                i += 1;
                TokenKind::Dollar
            }
            '#' => {
                i += 1;
                TokenKind::Pound
            }
            _ => {
                return Err(Diagnostic {
                    span: Span::new(start, i + 1),
                    message: format!("unexpected character '{}'", c),
                })
            }
        };
        previous_terminates = matches!(
            kind,
            TokenKind::Ident(_)
                | TokenKind::Integer(_)
                | TokenKind::Float(_)
                | TokenKind::String(_)
                | TokenKind::Char(_)
                | TokenKind::True
                | TokenKind::False
                | TokenKind::RParen
                | TokenKind::RBracket
                | TokenKind::RBrace
                | TokenKind::Question
        );
        out.push(Token {
            kind,
            span: Span::new(start, i),
        });
    }
    out.push(Token {
        kind: TokenKind::Eof,
        span: Span::new(i, i),
    });
    Ok(out)
}
