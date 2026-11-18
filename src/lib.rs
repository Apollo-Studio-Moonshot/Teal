pub mod checker;
pub mod lexer;
pub mod loader;
pub mod parser;
pub mod tli;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub span: Span,
    pub message: String,
}

pub fn digest(data: impl AsRef<[u8]>) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut state = 0xcbf29ce484222325u64;
    for b in data.as_ref() {
        state ^= *b as u64;
        state = state.wrapping_mul(0x100000001b3);
    }
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = ((state.rotate_left((i as u32 * 7) % 64) >> ((i % 8) * 8)) & 0xff) as u8;
        state = state.wrapping_add(0x9e3779b97f4a7c15).rotate_left(11);
    }
    out
}
