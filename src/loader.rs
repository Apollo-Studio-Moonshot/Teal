use crate::checker::Checker;
use crate::parser::parse;
use crate::tli;
use crate::Diagnostic;
use std::fs;
use std::path::{Path, PathBuf};

pub fn check_path(path: &Path) -> Result<String, Vec<Diagnostic>> {
    let files = collect_tl_files(path);
    if files.is_empty() {
        return Ok(format!("No .tl files found under {}", path.display()));
    }
    let mut checker = Checker::default();
    let mut count = 0;
    let mut errors = Vec::new();
    for file in files {
        let source = fs::read_to_string(&file).map_err(|e| {
            vec![Diagnostic {
                span: crate::Span::new(0, 0),
                message: e.to_string(),
            }]
        })?;
        match parse(&source) {
            Ok(module) => {
                count += 1;
                errors.extend(checker.check(&module));
            }
            Err(mut ds) => errors.append(&mut ds),
        }
    }
    if errors.is_empty() {
        Ok(format!("Checked {} .tl file(s)", count))
    } else {
        Err(errors)
    }
}

pub fn build_path(path: &Path) -> Result<String, Vec<Diagnostic>> {
    let files = collect_tl_files(path);
    if files.is_empty() {
        return Ok(format!("No .tl files found under {}", path.display()));
    }
    let base = if path.is_file() {
        path.parent().unwrap_or(Path::new("."))
    } else {
        path
    };
    let cache = base.join("target").join("teal");
    fs::create_dir_all(&cache).map_err(|e| {
        vec![Diagnostic {
            span: crate::Span::new(0, 0),
            message: e.to_string(),
        }]
    })?;
    let mut checker = Checker::default();
    let mut emitted = 0;
    let mut errors = Vec::new();
    for file in files {
        let source = fs::read_to_string(&file).map_err(|e| {
            vec![Diagnostic {
                span: crate::Span::new(0, 0),
                message: e.to_string(),
            }]
        })?;
        match parse(&source) {
            Ok(module) => {
                errors.extend(checker.check(&module));
                let summary = tli::emit(&module, &source);
                let bytes = tli::encode(&summary).map_err(|e| {
                    vec![Diagnostic {
                        span: crate::Span::new(0, 0),
                        message: e.to_string(),
                    }]
                })?;
                let name = module.package.clone().unwrap_or_else(|| {
                    file.file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string()
                });
                let out = cache.join(format!("{}.tli", name.replace('.', "_")));
                let unchanged = fs::read(&out).ok().is_some_and(|old| old == bytes);
                if !unchanged {
                    fs::write(&out, bytes).map_err(|e| {
                        vec![Diagnostic {
                            span: crate::Span::new(0, 0),
                            message: e.to_string(),
                        }]
                    })?;
                    emitted += 1;
                }
            }
            Err(mut ds) => errors.append(&mut ds),
        }
    }
    if errors.is_empty() {
        Ok(format!(
            "Built {} package summary/summaries ({} emitted)",
            emitted.min(1),
            emitted
        ))
    } else {
        Err(errors)
    }
}

fn collect_tl_files(path: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if path.is_file() {
        if path.extension().and_then(|e| e.to_str()) == Some("tl") {
            files.push(path.to_path_buf());
        }
    } else if fs::read_dir(path).is_ok() {
        fn visit(dir: &Path, files: &mut Vec<PathBuf>) {
            if let Ok(entries) = fs::read_dir(dir) {
                let mut entries: Vec<_> =
                    entries.filter_map(Result::ok).map(|e| e.path()).collect();
                entries.sort();
                for entry in entries {
                    if entry.is_dir() {
                        if entry.file_name().and_then(|n| n.to_str()) != Some("target") {
                            visit(&entry, files);
                        }
                    } else if entry.extension().and_then(|e| e.to_str()) == Some("tl") {
                        files.push(entry);
                    }
                }
            }
        }
        visit(path, &mut files);
    }
    files.sort();
    files
}
