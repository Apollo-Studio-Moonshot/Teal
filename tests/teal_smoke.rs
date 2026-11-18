use std::fs;
use std::path::PathBuf;
use teal::checker::check_package;
use teal::lexer::{lex, TokenKind};
use teal::macros::prelude_macros;
use teal::parser::parse_module;
use teal::tli::{self, Tli};
use teal::types::parse_type;

fn check_source(source: &str) -> Vec<teal::Diagnostic> {
    let module = parse_module(source, 0, "test", &prelude_macros()).expect("source should parse");
    check_package("test", &[module], &[]).diagnostics
}

fn codes(diagnostics: &[teal::Diagnostic]) -> Vec<&'static str> {
    diagnostics.iter().map(|d| d.code).collect()
}

fn temp_dir(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("teal-test-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn lexer_inserts_newline_semicolons() {
    let tokens = lex("let x = 1\nlet y = 2", 0).unwrap();
    assert_eq!(
        tokens.iter().filter(|t| t.kind == TokenKind::Semi).count(),
        1
    );
}

#[test]
fn lexer_does_not_terminate_before_closing_brace() {
    let tokens = lex("{\n  value\n}", 0).unwrap();
    assert!(!tokens.iter().any(|t| t.kind == TokenKind::Semi));
}

#[test]
fn parses_design_style_module() {
    let source = r#"
package http.cache
import std.io
import std.strings as str
import std.collections.{HashMap, MapIter}

pub const MAX_ENTRIES: usize = 1 << 16

struct Point {
    x: f64,
    y: f64,
}

enum Shape {
    Circle { radius: f64 },
    Rect { w: f64, h: f64 },
    Empty,
}

pub fn classify(s: Shape) -> string {
    return match s {
        Shape::Circle { radius } if radius > 100.0 => "big circle",
        Shape::Circle { .. } => "circle",
        Shape::Rect { .. } => "rect",
        Shape::Empty => "empty",
    };
}
"#;
    let module = parse_module(source, 0, "http.cache", &prelude_macros()).unwrap();
    assert_eq!(module.package.as_deref(), Some("http.cache"));
    assert_eq!(module.imports.len(), 3);
    assert!(module.items.len() >= 4);
}

#[test]
fn example_program_checks_cleanly() {
    let source = include_str!("../examples/hello.tl");
    let diagnostics = check_source(source);
    assert!(diagnostics.is_empty(), "{:#?}", diagnostics);
}

#[test]
fn detects_non_exhaustive_match() {
    let source = r#"
enum E { A, B }
fn f(e: E) -> i32 {
    return match e { E::A => 1 };
}
"#;
    let diagnostics = check_source(source);
    assert!(codes(&diagnostics).contains(&teal::codes::NON_EXHAUSTIVE));
}

#[test]
fn accepts_exhaustive_nested_match() {
    let source = r#"
enum E { A, B }
fn f(e: Option<E>) -> i32 {
    return match e {
        Some(E::A) => 1,
        Some(E::B) => 2,
        None => 3,
    };
}
"#;
    let diagnostics = check_source(source);
    assert!(diagnostics.is_empty(), "{:#?}", diagnostics);
}

#[test]
fn detects_type_mismatch() {
    let source = r#"
fn f() -> i32 {
    return "nope";
}
"#;
    let diagnostics = check_source(source);
    assert!(codes(&diagnostics).contains(&teal::codes::TYPE_MISMATCH));
}

#[test]
fn question_mark_uses_from_impl() {
    let source = r#"
enum E { Bad }

impl From<string> for E {
    fn from(_m: string) -> E { return E::Bad; }
}

fn raw() -> Result<i32, string> {
    return Ok(1);
}

fn converted() -> Result<i32, E> {
    let value = raw()?;
    return Ok(value);
}
"#;
    let diagnostics = check_source(source);
    assert!(diagnostics.is_empty(), "{:#?}", diagnostics);
}

#[test]
fn question_mark_reports_missing_conversion() {
    let source = r#"
enum E { Bad }
fn raw() -> Result<i32, string> {
    return Ok(1);
}
fn converted() -> Result<i32, E> {
    let value = raw()?;
    return Ok(value);
}
"#;
    let diagnostics = check_source(source);
    assert!(codes(&diagnostics).contains(&teal::codes::QUESTION_MARK));
}

#[test]
fn rejects_assignment_to_immutable_binding() {
    let source = r#"
fn f() -> i32 {
    let x = 1;
    x = 2;
    return x;
}
"#;
    let diagnostics = check_source(source);
    assert!(codes(&diagnostics).contains(&teal::codes::ASSIGN_IMMUTABLE));
}

#[test]
fn generic_function_requires_inferable_arguments() {
    let source = r#"
fn identity<T>(value: T) -> T {
    return value;
}
fn f() -> i32 {
    return identity(1);
}
"#;
    // The literal supplies `T = i32`, so this is accepted.
    assert!(check_source(source).is_empty());
}

#[test]
fn generic_bound_is_checked_at_call_site() {
    let source = r#"
interface Show { fn show(self: &Self) -> string }
struct NoShow { x: i32 }
fn render<T: Show>(value: T) -> string {
    return value.show();
}
fn f(value: NoShow) -> string {
    return render(value);
}
"#;
    let diagnostics = check_source(source);
    assert!(codes(&diagnostics).contains(&teal::codes::BOUND_NOT_SATISFIED));
}

#[test]
fn derive_emits_impls() {
    let source = r#"
#[derive(Eq, Hash, Show)]
struct Point { x: i32, y: i32 }
fn f(a: Point, b: Point) -> bool {
    return a.eq(&b);
}
"#;
    let diagnostics = check_source(source);
    assert!(diagnostics.is_empty(), "{:#?}", diagnostics);
}

#[test]
fn tli_round_trips_and_is_deterministic() {
    let source = r#"
package demo
pub struct Point { x: i32, y: i32 }
pub enum Shape { Circle { radius: f64 }, Empty }
pub fn classify(s: Shape) -> string { return "x"; }
"#;
    let module = parse_module(source, 0, "demo", &prelude_macros()).unwrap();
    let result = check_package("demo", &[module], &[]);
    assert!(result.diagnostics.is_empty(), "{:#?}", result.diagnostics);
    let summary = tli::emit(&result.env, Vec::new(), [0u8; 32]);
    let bytes = summary.encode();
    let decoded = Tli::decode(&bytes).unwrap();
    assert_eq!(decoded.package, "demo");
    assert_eq!(decoded.content_hash, summary.content_hash);
    assert_eq!(bytes, decoded.encode());
    assert_eq!(summary.content_hash, summary.compute_content_hash());
}

#[test]
fn type_strings_round_trip() {
    for text in [
        "i32",
        "Vec<Option<i32>>",
        "&mut [i32]",
        "(i32, bool)",
        "fn(i32) -> bool",
        "dyn Iterator<Item = i64>",
    ] {
        assert_eq!(parse_type(text).unwrap().to_string(), text);
    }
}

#[test]
fn build_is_incremental_and_writes_summaries() {
    let dir = temp_dir("incremental");
    let file = dir.join("main.tl");
    fs::write(
        &file,
        "package demo\npub fn add(a: i32, b: i32) -> i32 { return a + b; }\n",
    )
    .unwrap();

    let options = teal::loader::BuildOptions {
        write: true,
        ..Default::default()
    };
    let first = teal::loader::build(&dir, options.clone()).unwrap();
    assert_eq!(first.cached, 0);
    let artifact = dir.join("target").join("teal").join("demo.tli");
    let fingerprint = dir.join("target").join("teal").join("demo.fp");
    assert!(artifact.exists());
    assert!(fingerprint.exists());

    let second = teal::loader::build(&dir, options.clone()).unwrap();
    assert_eq!(second.cached, 1);

    // Body-only changes invalidate the package but reproduce a stable body
    // digest is not required; the package itself must re-check.
    fs::write(
        &file,
        "package demo\npub fn add(a: i32, b: i32) -> i32 { return b + a; }\n",
    )
    .unwrap();
    let third = teal::loader::build(&dir, options).unwrap();
    assert_eq!(third.cached, 0);

    let decoded = Tli::decode(&fs::read(&artifact).unwrap()).unwrap();
    assert_eq!(decoded.package, "demo");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn multi_package_build_propagates_summaries() {
    let root = temp_dir("workspace");
    let math = root.join("math");
    let app = root.join("app");
    fs::create_dir_all(&math).unwrap();
    fs::create_dir_all(&app).unwrap();
    fs::write(math.join("teal.pkg"), "name = acme.math\n").unwrap();
    fs::write(app.join("teal.pkg"), "name = acme.app\n").unwrap();
    fs::write(
        math.join("math.tl"),
        r#"
package acme.math
pub struct Point { x: i64, y: i64 }
pub fn add(a: i64, b: i64) -> i64 { return a + b; }
pub fn origin() -> Point { return Point { x: 0, y: 0 }; }
"#,
    )
    .unwrap();
    fs::write(
        app.join("app.tl"),
        r#"
package acme.app
import acme.math
pub fn run() -> i64 {
    let p: math::Point = math::origin();
    return math::add(p.x, p.y);
}
"#,
    )
    .unwrap();

    let options = teal::loader::BuildOptions {
        write: true,
        ..Default::default()
    };
    let report = teal::loader::build(&root, options.clone()).unwrap();
    assert_eq!(report.packages.len(), 2);

    // Body-only change in the dependency: its summary is unchanged, so the
    // dependent package must remain cached.
    fs::write(
        math.join("math.tl"),
        r#"
package acme.math
pub struct Point { x: i64, y: i64 }
pub fn add(a: i64, b: i64) -> i64 { return b + a; }
pub fn origin() -> Point { return Point { x: 0, y: 0 }; }
"#,
    )
    .unwrap();
    let report = teal::loader::build(&root, options).unwrap();
    let app = report
        .packages
        .iter()
        .find(|p| p.name == "acme.app")
        .unwrap();
    assert!(app.cached);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn unknown_import_is_reported() {
    let dir = temp_dir("unknown-import");
    fs::write(
        dir.join("main.tl"),
        "package demo\nimport missing.pkg\nfn f() -> i32 { return 1; }\n",
    )
    .unwrap();
    let error = teal::loader::build(&dir, Default::default()).unwrap_err();
    assert!(!error.diagnostics.is_empty());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn open_enums_require_a_wildcard_arm() {
    let source = r#"
open enum E { A }
fn ok(e: E) -> i32 {
    return match e { E::A => 1, _ => 0 };
}
fn bad(e: E) -> i32 {
    return match e { E::A => 1 };
}
"#;
    let diagnostics = check_source(source);
    assert_eq!(
        diagnostics
            .iter()
            .filter(|d| d.code == teal::codes::NON_EXHAUSTIVE)
            .count(),
        1
    );
}

#[test]
fn interface_provided_methods_are_checked_once_and_callable() {
    let source = r#"
interface Iterator {
    type Item
    fn next(self: &mut Self) -> Option<Self::Item>

    fn count(self: &mut Self) -> usize {
        let mut n: usize = 0;
        while self.next().is_some() {
            n += 1;
        }
        return n;
    }
}

struct C { cur: i64, end: i64 }

impl Iterator for C {
    type Item = i64
    fn next(self: &mut Self) -> Option<i64> {
        if self.cur >= self.end { return None; }
        let v = self.cur;
        self.cur += 1;
        return Some(v);
    }
}

fn total() -> usize {
    let mut c = C { cur: 0, end: 3 };
    return c.count();
}
"#;
    let diagnostics = check_source(source);
    assert!(diagnostics.is_empty(), "{:#?}", diagnostics);
}

#[test]
fn let_else_must_diverge() {
    let good = r#"
fn f(x: Option<i32>) -> i32 {
    let Some(v) = x else { return 0 };
    return v;
}
"#;
    assert!(check_source(good).is_empty());

    let bad = r#"
fn f(x: Option<i32>) -> i32 {
    let Some(v) = x else { let y = 1; };
    return v;
}
"#;
    let diagnostics = check_source(bad);
    assert!(codes(&diagnostics).contains(&teal::codes::LET_ELSE_DIVERGE));
}

#[test]
fn range_patterns_are_supported() {
    let source = r#"
fn f(x: i32) -> string {
    return match x {
        0..=9 => "digit",
        _ => "other",
    };
}
"#;
    let diagnostics = check_source(source);
    assert!(diagnostics.is_empty(), "{:#?}", diagnostics);
}

#[test]
fn dependency_macros_are_exported_in_summaries() {
    let root = temp_dir("macro-deps");
    let macros_pkg = root.join("macros_pkg");
    let user = root.join("user");
    fs::create_dir_all(&macros_pkg).unwrap();
    fs::create_dir_all(&user).unwrap();
    fs::write(macros_pkg.join("teal.pkg"), "name = acme.macros\n").unwrap();
    fs::write(user.join("teal.pkg"), "name = acme.user\n").unwrap();
    fs::write(
        macros_pkg.join("lib.tl"),
        r#"
package acme.macros
macro answer! { () => { 42 } }
"#,
    )
    .unwrap();
    fs::write(
        user.join("main.tl"),
        r#"
package acme.user
import acme.macros
fn f() -> i32 {
    return answer!();
}
"#,
    )
    .unwrap();

    let report = teal::loader::build(
        &root,
        teal::loader::BuildOptions {
            write: true,
            ..Default::default()
        },
    )
    .expect("dependency macro should be available downstream");
    assert_eq!(report.packages.len(), 2);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn external_impl_without_local_type_is_rejected() {
    let root = temp_dir("orphan");
    let base = root.join("base");
    let other = root.join("other");
    fs::create_dir_all(&base).unwrap();
    fs::create_dir_all(&other).unwrap();
    fs::write(base.join("teal.pkg"), "name = acme.base\n").unwrap();
    fs::write(other.join("teal.pkg"), "name = acme.other\n").unwrap();
    fs::write(
        base.join("base.tl"),
        r#"
package acme.base
pub interface Marker { fn mark(self: &Self) -> i32 }
pub struct Thing { x: i32 }
"#,
    )
    .unwrap();
    fs::write(
        other.join("other.tl"),
        r#"
package acme.other
import acme.base
impl base::Marker for base::Thing {
    fn mark(self: &Self) -> i32 { return 1; }
}
"#,
    )
    .unwrap();
    let error = teal::loader::build(&root, Default::default()).unwrap_err();
    assert!(error
        .diagnostics
        .iter()
        .any(|d| d.code == teal::codes::ORPHAN_IMPL));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn closures_check_against_expected_fn_types() {
    let source = r#"
fn apply(f: fn(i32) -> i32, x: i32) -> i32 {
    return f(x);
}
fn run() -> i32 {
    return apply(|x: i32| x + 1, 41);
}
"#;
    let diagnostics = check_source(source);
    assert!(diagnostics.is_empty(), "{:#?}", diagnostics);
}

#[test]
fn dyn_dispatch_uses_interface_methods() {
    let source = r#"
interface Show {
    fn show(self: &Self) -> string
}

struct P { x: i32 }

impl Show for P {
    fn show(self: &Self) -> string { return "p"; }
}

fn render(value: dyn Show) -> string {
    return value.show();
}
"#;
    let diagnostics = check_source(source);
    assert!(diagnostics.is_empty(), "{:#?}", diagnostics);
}

#[test]
fn generic_struct_literals_infer_from_expected_type() {
    let source = r#"
struct Box<T> { value: T }
fn make() -> Box<i32> {
    let b: Box<i32> = Box { value: 1 };
    return b;
}
"#;
    let diagnostics = check_source(source);
    assert!(diagnostics.is_empty(), "{:#?}", diagnostics);
}
