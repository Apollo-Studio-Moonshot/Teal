//! Package discovery, DAG scheduling, and the incremental build.
//!
//! Packages are directories of `.tl` files. A `teal.pkg` manifest may name the
//! package explicitly; otherwise the name comes from the file's `package`
//! declaration. Imports form the dependency DAG. Packages at the same level of
//! the DAG are compiled concurrently with `std::thread::scope`.
//!
//! Incremental builds follow DESIGN.md §4.1: a fingerprint over the compiler
//! version, ABI, target, sources, and dependency summary hashes is recorded
//! next to the `.tli`. If it matches, the package is a no-op and contributes
//! its cached summary to dependents. Artifacts are written only when they
//! differ, so a no-op rebuild does not touch the filesystem.

use crate::ast::Module;
use crate::checker::{self, Env};
use crate::codegen;
use crate::hash::{self, Digest};
use crate::lexer::{lex, TokenKind};
use crate::macros::{self, MacroDef};
use crate::parser::parse_module;
use crate::source::{CompileError, SourceMap};
use crate::tli::{self, Tli};
use crate::{codes, Diagnostic, FileId, Span};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default)]
pub struct BuildOptions {
    /// Write `.tli` summaries and fingerprints to the target directory.
    pub write: bool,
    /// Override the artifact directory. Defaults to `<root>/target/teal`.
    pub target: Option<PathBuf>,
    /// Maximum number of packages compiled concurrently. `None` means one
    /// task per ready package.
    pub jobs: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct PackageSummary {
    pub name: String,
    pub files: usize,
    pub lines: usize,
    pub functions: usize,
    pub cached: bool,
    pub deps: Vec<String>,
    pub content_hash: String,
    pub duration: Duration,
}

#[derive(Clone, Debug)]
pub struct BuildReport {
    pub packages: Vec<PackageSummary>,
    pub total: Duration,
    pub source_lines: usize,
    pub functions: usize,
    pub cached: usize,
    pub target_dir: PathBuf,
}

impl BuildReport {
    pub fn render(&self) -> String {
        let mut out = String::new();
        for pkg in &self.packages {
            out.push_str(&format!(
                "  {:<28} {:>4} file(s) {:>6} lines {:>4} fn  {:<9} {}  {}\n",
                pkg.name,
                pkg.files,
                pkg.lines,
                pkg.functions,
                if pkg.cached { "cached" } else { "checked" },
                &pkg.content_hash[..12.min(pkg.content_hash.len())],
                fmt_duration(pkg.duration),
            ));
        }
        out.push_str(&format!(
            "{} package(s), {} line(s), {} function(s), {} cached, {} total\n",
            self.packages.len(),
            self.source_lines,
            self.functions,
            self.cached,
            fmt_duration(self.total)
        ));
        out
    }
}

fn fmt_duration(duration: Duration) -> String {
    if duration.as_millis() == 0 {
        format!("{}µs", duration.as_micros())
    } else {
        format!("{}ms", duration.as_millis())
    }
}

pub fn check(path: &Path) -> Result<BuildReport, CompileError> {
    build(path, BuildOptions::default())
}

pub fn build_project(path: &Path, options: BuildOptions) -> Result<BuildReport, CompileError> {
    build(path, options)
}

pub fn build(path: &Path, options: BuildOptions) -> Result<BuildReport, CompileError> {
    let started = Instant::now();
    let root = if path.is_file() {
        path.parent().unwrap_or(Path::new(".")).to_path_buf()
    } else {
        path.to_path_buf()
    };
    let target_dir = options
        .target
        .clone()
        .unwrap_or_else(|| root.join("target").join("teal"));

    if !path.exists() {
        let mut sources = SourceMap::new();
        let id = sources.add(path.display().to_string(), Some(path), "");
        return Err(CompileError::new(
            vec![Diagnostic::error(
                codes::IO,
                Span::new(id, 0, 0),
                format!("path {} does not exist", path.display()),
            )],
            sources,
        ));
    }

    // 1. Discover package directories.
    let raw = discover(path);
    if raw.is_empty() {
        return Ok(BuildReport {
            packages: Vec::new(),
            total: started.elapsed(),
            source_lines: 0,
            functions: 0,
            cached: 0,
            target_dir,
        });
    }

    // 2. Read every source file, assigning deterministic file ids.
    let mut sources = SourceMap::new();
    let mut packages: Vec<InputPackage> = Vec::new();
    for dir in raw {
        let mut files = collect_tl_files(&dir);
        files.sort();
        if files.is_empty() {
            continue;
        }
        let manifest = read_manifest(&dir);
        let mut units = Vec::new();
        for file in files {
            let text = match fs::read_to_string(&file) {
                Ok(text) => text,
                Err(error) => {
                    let id = sources.add(file.display().to_string(), Some(&file), "");
                    return Err(CompileError::new(
                        vec![Diagnostic::error(
                            codes::IO,
                            Span::new(id, 0, 0),
                            format!("cannot read {}: {error}", file.display()),
                        )],
                        sources,
                    ));
                }
            };
            let id = sources.add(
                file.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| file.display().to_string()),
                Some(&file),
                text.clone(),
            );
            units.push(SourceUnit {
                id,
                path: file,
                text,
            });
        }
        packages.push(InputPackage {
            dir: dir.clone(),
            manifest_name: manifest,
            default_name: default_package_name(&root, &dir),
            name: String::new(),
            units,
            declared_name: None,
            imports: Vec::new(),
            deps: Vec::new(),
        });
    }
    packages.sort_by(|a, b| a.default_name.cmp(&b.default_name));

    // 3. Scan headers and settle package names.
    let mut diagnostics = Vec::new();
    for package in &mut packages {
        let mut declared: Option<(String, Span)> = None;
        for unit in &package.units {
            match scan_header(&unit.text, unit.id) {
                Ok(header) => {
                    if let Some((name, span)) = header.package {
                        match &declared {
                            Some((existing, _)) if existing != &name => {
                                diagnostics.push(Diagnostic::error(
                                    codes::DUPLICATE_DEF,
                                    span,
                                    format!(
                                        "package `{name}` conflicts with `{existing}` declared in another file of the same directory"
                                    ),
                                ));
                            }
                            None => declared = Some((name, span)),
                            _ => {}
                        }
                    }
                    package.imports.extend(header.imports);
                }
                Err(diag) => diagnostics.push(diag),
            }
        }
        let name = package
            .manifest_name
            .clone()
            .or_else(|| declared.as_ref().map(|(name, _)| name.clone()))
            .unwrap_or_else(|| package.default_name.clone());
        package.name = name;
        package.declared_name = declared.map(|(name, _)| name);
    }
    if !diagnostics.is_empty() {
        return Err(CompileError::new(diagnostics, sources));
    }

    // 4. Duplicate package names are a hard error (they make the DAG ambiguous).
    let mut by_name: BTreeMap<String, usize> = BTreeMap::new();
    for (index, package) in packages.iter().enumerate() {
        if let Some(previous) = by_name.insert(package.name.clone(), index) {
            let span = package
                .units
                .first()
                .map(|u| Span::new(u.id, 0, 0))
                .unwrap_or_else(Span::dummy);
            return Err(CompileError::new(
                vec![Diagnostic::error(
                    codes::DUPLICATE_DEF,
                    span,
                    format!(
                        "package name `{}` is used by both {} and {}",
                        package.name,
                        packages[previous].dir.display(),
                        package.dir.display()
                    ),
                )],
                sources,
            ));
        }
    }

    // 5. Resolve imports to package dependencies.
    for index in 0..packages.len() {
        let imports = packages[index].imports.clone();
        let mut deps = BTreeSet::new();
        for (import, span) in imports {
            if by_name.contains_key(&import) {
                deps.insert(import);
            } else if import.starts_with("std.") || import == "std" || import == "prelude" {
                // Bundled prelude; no manifest needed.
            } else {
                return Err(CompileError::new(
                    vec![Diagnostic::error(
                        codes::UNKNOWN_NAME,
                        span,
                        format!("imported package `{import}` was not found in this workspace"),
                    )
                    .with_note(
                        "add a directory containing `teal.pkg` for it under the project root",
                    )],
                    sources,
                ));
            }
        }
        packages[index].deps = deps.into_iter().collect();
    }

    // 6. Build levels of the DAG and compile packages concurrently.
    let mut artifacts: BTreeMap<String, Arc<Artifact>> = BTreeMap::new();
    let mut pending: BTreeSet<String> = packages.iter().map(|p| p.name.clone()).collect();
    let mut summaries: Vec<PackageSummary> = Vec::new();
    let mut errors: Vec<Diagnostic> = Vec::new();
    let mut cached_count = 0;

    while !pending.is_empty() {
        let ready: Vec<usize> = packages
            .iter()
            .enumerate()
            .filter(|(_, package)| {
                pending.contains(&package.name)
                    && package.deps.iter().all(|dep| artifacts.contains_key(dep))
            })
            .map(|(index, _)| index)
            .collect();
        if ready.is_empty() {
            let span = packages
                .iter()
                .find(|p| pending.contains(&p.name))
                .and_then(|p| p.units.first())
                .map(|u| Span::new(u.id, 0, 0))
                .unwrap_or(Span::dummy());
            errors.push(
                Diagnostic::error(
                    codes::PARSE,
                    span,
                    "dependency cycle detected between packages",
                )
                .with_note("Teal package graphs must be acyclic"),
            );
            break;
        }

        let mut results: Vec<(usize, Result<Artifact, Vec<Diagnostic>>)> = Vec::new();
        let mut remaining = ready;
        while !remaining.is_empty() {
            let take = options
                .jobs
                .filter(|jobs| *jobs > 0)
                .map(|jobs| jobs.min(remaining.len()))
                .unwrap_or(remaining.len());
            let chunk: Vec<usize> = remaining.drain(..take).collect();
            let chunk_results = std::thread::scope(|scope| {
                let mut handles = Vec::new();
                for index in &chunk {
                    let package = &packages[*index];
                    let mut dep_tlis = Vec::new();
                    let mut dep_hashes = Vec::new();
                    for dep in &package.deps {
                        if let Some(artifact) = artifacts.get(dep) {
                            dep_tlis.push(artifact.tli.clone());
                            dep_hashes.push((dep.clone(), artifact.tli.content_hash));
                        }
                    }
                    let target_dir = target_dir.clone();
                    let write = options.write;
                    handles.push(scope.spawn(move || {
                        compile_package(package, dep_tlis, dep_hashes, &target_dir, write)
                    }));
                }
                handles
                    .into_iter()
                    .map(|handle| {
                        handle.join().unwrap_or_else(|_| {
                            Err(vec![Diagnostic::error(
                                codes::PARSE,
                                Span::dummy(),
                                "package compilation panicked",
                            )])
                        })
                    })
                    .collect::<Vec<_>>()
            });
            results.extend(chunk.into_iter().zip(chunk_results));
        }

        for (index, result) in results {
            let package = &packages[index];
            pending.remove(&package.name);
            match result {
                Ok(artifact) => {
                    if artifact.cached {
                        cached_count += 1;
                    }
                    summaries.push(PackageSummary {
                        name: package.name.clone(),
                        files: package.units.len(),
                        lines: artifact.lines,
                        functions: artifact.functions,
                        cached: artifact.cached,
                        deps: package.deps.clone(),
                        content_hash: hash::to_hex(&artifact.tli.content_hash),
                        duration: artifact.duration,
                    });
                    artifacts.insert(package.name.clone(), Arc::new(artifact));
                }
                Err(mut diags) => errors.append(&mut diags),
            }
        }
        if !errors.is_empty() {
            break;
        }
    }

    if !errors.is_empty() {
        return Err(CompileError::new(errors, sources));
    }

    summaries.sort_by(|a, b| a.name.cmp(&b.name));
    let source_lines = summaries.iter().map(|s| s.lines).sum();
    let functions = summaries.iter().map(|s| s.functions).sum();

    Ok(BuildReport {
        packages: summaries,
        total: started.elapsed(),
        source_lines,
        functions,
        cached: cached_count,
        target_dir,
    })
}

struct SourceUnit {
    id: FileId,
    path: PathBuf,
    text: String,
}

struct InputPackage {
    dir: PathBuf,
    manifest_name: Option<String>,
    default_name: String,
    name: String,
    declared_name: Option<String>,
    units: Vec<SourceUnit>,
    imports: Vec<(String, Span)>,
    deps: Vec<String>,
}

struct Artifact {
    tli: Tli,
    lines: usize,
    functions: usize,
    cached: bool,
    duration: Duration,
}

struct Header {
    package: Option<(String, Span)>,
    imports: Vec<(String, Span)>,
}

fn scan_header(source: &str, file: FileId) -> Result<Header, Diagnostic> {
    let tokens = lex(source, file)?;
    let mut pos = 0;
    let mut package = None;
    let mut imports = Vec::new();

    let read_path = |tokens: &[crate::lexer::Token], pos: &mut usize| -> (String, Span) {
        let mut parts = Vec::new();
        let mut span = tokens.get(*pos).map(|t| t.span).unwrap_or(Span::dummy());
        while *pos < tokens.len() {
            match &tokens[*pos].kind {
                TokenKind::Ident(part) => {
                    parts.push(part.clone());
                    span = span.merge(tokens[*pos].span);
                    *pos += 1;
                }
                TokenKind::Dot | TokenKind::ColonColon => {
                    *pos += 1;
                }
                _ => break,
            }
        }
        (parts.join("."), span)
    };

    loop {
        while pos < tokens.len()
            && matches!(tokens[pos].kind, TokenKind::Semi | TokenKind::DocComment(_))
        {
            pos += 1;
        }
        match tokens.get(pos).map(|t| &t.kind) {
            Some(TokenKind::Package) => {
                pos += 1;
                let (name, span) = read_path(&tokens, &mut pos);
                if !name.is_empty() {
                    package = Some((name, span));
                }
            }
            Some(TokenKind::Import) => {
                pos += 1;
                let start = tokens[pos].span;
                let (name, span) = read_path(&tokens, &mut pos);
                // Skip a braced import list.
                if tokens.get(pos).is_some_and(|t| t.kind == TokenKind::LBrace) {
                    while pos < tokens.len() && tokens[pos].kind != TokenKind::RBrace {
                        pos += 1;
                    }
                    pos += 1;
                }
                if !name.is_empty() {
                    imports.push((name, start.merge(span)));
                }
            }
            _ => break,
        }
    }
    Ok(Header { package, imports })
}

fn read_manifest(dir: &Path) -> Option<String> {
    let path = dir.join("teal.pkg");
    let text = fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "name" {
                let value = value.trim().trim_matches('"').trim_matches('\'');
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

fn default_package_name(root: &Path, dir: &Path) -> String {
    let relative = dir.strip_prefix(root).unwrap_or(dir);
    if relative.as_os_str().is_empty() {
        return dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "root".into());
    }
    relative
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join(".")
}

/// Discover package directories. A package is a directory containing `.tl`
/// files or a `teal.pkg` manifest; nested packages are not merged into parents.
fn discover(path: &Path) -> Vec<PathBuf> {
    if path.is_file() {
        return path
            .parent()
            .map(|p| vec![p.to_path_buf()])
            .unwrap_or_default();
    }
    if !path.is_dir() {
        return Vec::new();
    }

    let has_manifest = path.join("teal.pkg").is_file();
    let has_tl = fs::read_dir(path)
        .map(|entries| {
            entries.filter_map(Result::ok).any(|e| {
                e.file_type().map(|t| t.is_file()).unwrap_or(false)
                    && e.path().extension().and_then(|e| e.to_str()) == Some("tl")
            })
        })
        .unwrap_or(false);
    if has_manifest || has_tl {
        return vec![path.to_path_buf()];
    }

    // Workspace: any directory containing `teal.pkg` is a package.
    let mut packages = Vec::new();
    collect_manifest_dirs(path, &mut packages);
    if !packages.is_empty() {
        packages.sort();
        return packages;
    }

    // Fallback: directories that directly contain `.tl` files.
    collect_tl_dirs(path, &mut packages);
    packages.sort();
    packages
}

fn collect_manifest_dirs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    for entry in entries {
        if !entry.is_dir() || entry.file_name().and_then(|n| n.to_str()) == Some("target") {
            continue;
        }
        if entry.join("teal.pkg").is_file() {
            out.push(entry);
            continue;
        }
        collect_manifest_dirs(&entry, out);
    }
}

fn collect_tl_dirs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    let mut has_tl = false;
    for entry in &entries {
        if entry.is_file() && entry.extension().and_then(|e| e.to_str()) == Some("tl") {
            has_tl = true;
        }
    }
    if has_tl {
        out.push(dir.to_path_buf());
    }
    for entry in entries {
        if entry.is_dir() && entry.file_name().and_then(|n| n.to_str()) != Some("target") {
            collect_tl_dirs(&entry, out);
        }
    }
}

fn collect_tl_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_tl_files_inner(dir, &mut files);
    files.sort();
    files
}

fn collect_tl_files_inner(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    for entry in entries {
        if entry.is_dir() {
            let name = entry.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "target" {
                continue;
            }
            // Nested package boundaries belong to other packages.
            if entry.join("teal.pkg").is_file() {
                continue;
            }
            collect_tl_files_inner(&entry, out);
        } else if entry.extension().and_then(|e| e.to_str()) == Some("tl") {
            out.push(entry);
        }
    }
}

fn compile_package(
    package: &InputPackage,
    dep_tlis: Vec<Tli>,
    dep_hashes: Vec<(String, Digest)>,
    target_dir: &Path,
    write: bool,
) -> Result<Artifact, Vec<Diagnostic>> {
    let started = Instant::now();
    let source_hash = hash_sources(&package.units);
    let fingerprint = tli::fingerprint(source_hash, &dep_hashes);
    let artifact_base = target_dir.join(format!(
        "{}.tli",
        package.name.replace(['.', '/', '\\'], "_")
    ));
    let fp_path = artifact_base.with_extension("fp");

    // Incremental hit: fingerprint matches and the summary decodes.
    if write {
        if let (Ok(fp_text), Ok(bytes)) = (fs::read_to_string(&fp_path), fs::read(&artifact_base)) {
            if fp_text.trim() == hash::to_hex(&fingerprint) {
                if let Ok(mut tli) = Tli::decode(&bytes) {
                    if tli.package == package.name {
                        tli.fingerprint = fingerprint;
                        let functions = tli.functions.len()
                            + tli.impls.iter().map(|i| i.methods.len()).sum::<usize>();
                        return Ok(Artifact {
                            lines: package.units.iter().map(|u| u.text.lines().count()).sum(),
                            functions,
                            cached: true,
                            tli,
                            duration: started.elapsed(),
                        });
                    }
                }
            }
        }
    }

    // Dependency macros are available while parsing this package.
    let mut macro_table: BTreeMap<String, MacroDef> = macros::prelude_macros();
    for tli in &dep_tlis {
        for saved in &tli.macros {
            let def = macros::macro_def_from_rules(
                &saved.name,
                &saved.rules,
                Span::dummy().file,
                &tli.package,
            );
            macro_table.insert(saved.name.clone(), def);
        }
    }

    let mut modules: Vec<Module> = Vec::new();
    let mut diagnostics = Vec::new();
    for unit in &package.units {
        match parse_module(&unit.text, unit.id, &package.name, &macro_table) {
            Ok(module) => {
                if let Some(declared) = &module.package {
                    if declared != &package.name {
                        if package.declared_name.as_deref() == Some(declared) {
                            diagnostics.push(
                                Diagnostic::error(
                                    codes::PARSE,
                                    module.package_span.unwrap_or(Span::dummy()),
                                    format!(
                                        "this file declares package `{declared}`, but the package is named `{}`",
                                        package.name
                                    ),
                                )
                                .with_note("the manifest `name` or directory governs the package name"),
                            );
                        }
                    }
                }
                modules.push(module);
            }
            Err(mut errs) => diagnostics.append(&mut errs),
        }
    }
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }

    let result = checker::check_package(&package.name, &modules, &dep_tlis);
    diagnostics.extend(result.diagnostics);
    if diagnostics.iter().any(Diagnostic::is_error) {
        return Err(diagnostics);
    }

    let mut tli = tli::emit(&result.env, dep_hashes, fingerprint);
    tli.builtin = false;

    if write {
        let lowered = codegen::lower(&package.name, &modules, &result.env);
        if let Err(diagnostics) = codegen::emit_object(target_dir, &lowered) {
            return Err(diagnostics);
        }
        if let Some(parent) = artifact_base.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                return Err(vec![Diagnostic::error(
                    codes::IO,
                    Span::dummy(),
                    format!("cannot create {}: {error}", parent.display()),
                )]);
            }
        }
        let bytes = tli.encode();
        let unchanged = fs::read(&artifact_base)
            .ok()
            .is_some_and(|old| old == bytes);
        if !unchanged {
            if let Err(error) = write_atomic(&artifact_base, &bytes) {
                return Err(vec![Diagnostic::error(
                    codes::IO,
                    Span::dummy(),
                    format!("cannot write {}: {error}", artifact_base.display()),
                )]);
            }
        }
        let _ = write_atomic(&fp_path, hash::to_hex(&fingerprint).as_bytes());

        // Docs live in a companion file so editing them never re-checks code.
        let docs_path = artifact_base.with_extension("tld");
        let docs = render_docs(&tli);
        let _ = write_atomic(&docs_path, docs.as_bytes());
    }

    Ok(Artifact {
        lines: package.units.iter().map(|u| u.text.lines().count()).sum(),
        functions: count_functions(&result.env),
        cached: false,
        tli,
        duration: started.elapsed(),
    })
}

fn count_functions(env: &Env) -> usize {
    env.functions
        .values()
        .filter(|f| f.package == env.package && f.public)
        .count()
        + env
            .impls
            .iter()
            .filter(|i| i.package == env.package)
            .map(|i| i.methods.len())
            .sum::<usize>()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if fs::read(path).ok().as_deref() == Some(bytes) {
        return Ok(());
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

fn render_docs(tli: &Tli) -> String {
    let mut out = String::new();
    for (key, docs) in &tli.docs {
        out.push_str(&format!("## {key}\n{docs}\n\n"));
    }
    out
}

fn hash_sources(units: &[SourceUnit]) -> Digest {
    let mut data = Vec::new();
    for unit in units {
        data.extend_from_slice(unit.path.to_string_lossy().as_bytes());
        data.push(0);
        data.extend_from_slice(unit.text.as_bytes());
        data.push(0);
    }
    hash::sha256(&data)
}

/// Load all `.tli` summaries produced under a target directory. Useful for
/// tooling and tests.
pub fn load_summaries(target_dir: &Path) -> BTreeMap<String, Tli> {
    let mut out = BTreeMap::new();
    let Ok(entries) = fs::read_dir(target_dir) else {
        return out;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("tli") {
            if let Ok(bytes) = fs::read(&path) {
                if let Ok(tli) = Tli::decode(&bytes) {
                    out.insert(tli.package.clone(), tli);
                }
            }
        }
    }
    out
}
