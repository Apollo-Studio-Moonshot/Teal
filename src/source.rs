//! Source management and diagnostic rendering.
//!
//! Every diagnostic carries a `Span` with a file id; the loader owns the
//! `SourceMap`, so errors from all packages can be rendered together with
//! line/column context and a caret.

use crate::{Diagnostic, FileId, Severity};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct SourceFile {
    pub id: FileId,
    pub name: String,
    pub path: Option<PathBuf>,
    pub text: String,
    line_starts: Vec<usize>,
}

impl SourceFile {
    fn new(id: FileId, name: String, path: Option<PathBuf>, text: String) -> Self {
        let mut line_starts = vec![0];
        for (i, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self {
            id,
            name,
            path,
            text,
            line_starts,
        }
    }

    /// 1-based line and column for a byte offset.
    pub fn line_col(&self, offset: u32) -> (usize, usize) {
        let offset = (offset as usize).min(self.text.len());
        let line = match self.line_starts.binary_search(&offset) {
            Ok(line) => line,
            Err(next) => next.saturating_sub(1),
        };
        (line + 1, offset - self.line_starts[line] + 1)
    }

    pub fn line_text(&self, line: usize) -> &str {
        let start = self.line_starts[line.saturating_sub(1)];
        let end = self
            .line_starts
            .get(line)
            .copied()
            .unwrap_or(self.text.len());
        self.text[start..end].trim_end_matches(['\n', '\r'])
    }

    pub fn display_name(&self) -> String {
        self.path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| self.name.clone())
    }
}

#[derive(Clone, Debug, Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(
        &mut self,
        name: impl Into<String>,
        path: Option<&Path>,
        text: impl Into<String>,
    ) -> FileId {
        let id = self.files.len() as FileId;
        let path = path.map(Path::to_path_buf);
        self.files
            .push(SourceFile::new(id, name.into(), path, text.into()));
        id
    }

    pub fn file(&self, id: FileId) -> Option<&SourceFile> {
        self.files.get(id as usize)
    }

    pub fn files(&self) -> &[SourceFile] {
        &self.files
    }

    pub fn name(&self, id: FileId) -> String {
        self.file(id)
            .map(SourceFile::display_name)
            .unwrap_or_else(|| format!("<file {}>", id))
    }

    /// Render a diagnostic with a rustc-style snippet.
    pub fn render(&self, diag: &Diagnostic) -> String {
        let mut out = String::new();
        let severity = match diag.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        out.push_str(&format!("{severity}[{}]: {}\n", diag.code, diag.message));

        let Some(file) = self.file(diag.span.file) else {
            if !diag.notes.is_empty() {
                for note in &diag.notes {
                    out.push_str(&format!("  = note: {note}\n"));
                }
            }
            return out;
        };

        let (line, col) = file.line_col(diag.span.start);
        out.push_str(&format!("  --> {}:{}:{}\n", file.display_name(), line, col));
        if file.text.is_empty() {
            for note in &diag.notes {
                out.push_str(&format!("  = note: {note}\n"));
            }
            return out;
        }

        let gutter = line.to_string();
        let pad = " ".repeat(gutter.len());
        let source_line = file.line_text(line);
        out.push_str(&format!("{pad} |\n"));
        out.push_str(&format!("{gutter} | {source_line}\n"));

        let line_start = file.line_starts_at(line);
        let start = (diag.span.start as usize).clamp(line_start, file.text.len());
        let end = (diag.span.end as usize).clamp(start, file.text.len());
        let display_width = file.text[line_start..start].chars().count();
        let caret_width = (file.text[start..end].chars().count()).max(1);
        out.push_str(&format!(
            "{pad} | {}{}\n",
            " ".repeat(display_width),
            "^".repeat(caret_width)
        ));

        for note in &diag.notes {
            out.push_str(&format!("{pad} = note: {note}\n"));
        }
        out
    }

    pub fn render_all(&self, diagnostics: &[Diagnostic]) -> String {
        let mut out = String::new();
        for diag in diagnostics {
            out.push_str(&self.render(diag));
            out.push('\n');
        }
        out
    }
}

impl SourceFile {
    fn line_starts_at(&self, line: usize) -> usize {
        self.line_starts[line.saturating_sub(1)]
    }
}

/// A failure carrying diagnostics plus the source map needed to render them.
#[derive(Debug)]
pub struct CompileError {
    pub diagnostics: Vec<Diagnostic>,
    pub sources: SourceMap,
}

impl CompileError {
    pub fn new(mut diagnostics: Vec<Diagnostic>, sources: SourceMap) -> Self {
        crate::sort_diagnostics(&mut diagnostics);
        Self {
            diagnostics,
            sources,
        }
    }

    pub fn render(&self) -> String {
        self.sources.render_all(&self.diagnostics)
    }

    pub fn error_count(&self) -> usize {
        self.diagnostics.iter().filter(|d| d.is_error()).count()
    }
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render())
    }
}

impl std::error::Error for CompileError {}
