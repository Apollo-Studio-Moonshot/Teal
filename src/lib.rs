//! Teal — a fast-compiling, extensible static language.
//!
//! This crate implements the front end described in `DESIGN.md`: a hand-written
//! lexer with newline statement termination, an LL(1) recursive-descent parser,
//! hygienic declarative macro expansion, a per-function bidirectional type
//! checker, and a deterministic `.tli` interface summary format. There is no
//! code generator yet; the build pipeline is "check and emit summaries", which
//! is exactly the prototype slice suggested in section 7 of the design.

pub mod ast;
pub mod checker;
pub mod codegen;
pub mod hash;
pub mod lexer;
pub mod loader;
pub mod macros;
pub mod parser;
pub mod source;
pub mod tli;
pub mod types;

pub use source::{CompileError, SourceFile, SourceMap};

use std::fmt;

/// Bumped whenever the shape of compiled artifacts changes. Any change here
/// invalidates every cached package in the world (see DESIGN.md §5.4).
pub const FORMAT_VERSION: u32 = 2;

/// Bumped when the compiler semantics change in a way that should force a
/// rebuild without changing the artifact format.
pub const COMPILER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub const COMPILER_ABI: &str = "teal-0.2";

pub type FileId = u32;

/// A span into the source map. `start`/`end` are byte offsets.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Span {
    pub file: FileId,
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub const fn new(file: FileId, start: usize, end: usize) -> Self {
        Self {
            file,
            start: start as u32,
            end: end as u32,
        }
    }

    pub const fn dummy() -> Self {
        Self {
            file: 0,
            start: 0,
            end: 0,
        }
    }

    pub fn merge(self, other: Span) -> Span {
        if self.file != other.file {
            return self;
        }
        Span {
            file: self.file,
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    pub fn is_dummy(self) -> bool {
        self.start == 0 && self.end == 0
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}..{}", self.file, self.start, self.end)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: &'static str,
    pub span: Span,
    pub message: String,
    pub notes: Vec<String>,
}

impl Diagnostic {
    pub fn error(code: &'static str, span: Span, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code,
            span,
            message: message.into(),
            notes: Vec::new(),
        }
    }

    pub fn warning(code: &'static str, span: Span, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            code,
            span,
            message: message.into(),
            notes: Vec::new(),
        }
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// Convenience for constructing errors with an explicit code and span.
pub fn err(code: &'static str, span: Span, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(code, span, message)
}

/// Error codes are grouped by phase. They are stable identifiers intended for
/// tooling; the human-readable message is not.
pub mod codes {
    pub const LEX: &str = "E0001";
    pub const PARSE: &str = "E0002";
    pub const MACRO: &str = "E0003";
    pub const IO: &str = "E0004";

    pub const UNKNOWN_TYPE: &str = "E0101";
    pub const UNKNOWN_NAME: &str = "E0102";
    pub const DUPLICATE_DEF: &str = "E0103";

    pub const TYPE_MISMATCH: &str = "E0201";
    pub const WRONG_ARITY: &str = "E0202";
    pub const NOT_CALLABLE: &str = "E0203";
    pub const ASSIGN_IMMUTABLE: &str = "E0204";
    pub const BAD_OPERATOR: &str = "E0205";

    pub const UNKNOWN_FIELD: &str = "E0301";
    pub const MISSING_FIELD: &str = "E0302";
    pub const UNKNOWN_VARIANT: &str = "E0303";

    pub const UNKNOWN_METHOD: &str = "E0401";
    pub const MISSING_IMPL_METHOD: &str = "E0402";
    pub const MISSING_ASSOC_TYPE: &str = "E0403";
    pub const INVALID_IMPL: &str = "E0404";
    pub const BOUND_NOT_SATISFIED: &str = "E0405";
    pub const ORPHAN_IMPL: &str = "E0406";

    pub const NON_EXHAUSTIVE: &str = "E0501";
    pub const QUESTION_MARK: &str = "E0502";
    pub const LET_ELSE_DIVERGE: &str = "E0503";
}

/// Deterministically collect and sort diagnostics so output never depends on
/// traversal order or thread scheduling.
pub fn sort_diagnostics(diags: &mut [Diagnostic]) {
    diags.sort_by(|a, b| {
        (a.span.file, a.span.start, a.span.end, a.code, &a.message).cmp(&(
            b.span.file,
            b.span.start,
            b.span.end,
            b.code,
            &b.message,
        ))
    });
}
