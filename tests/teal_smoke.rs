use std::fs;
use teal::lexer::{lex, TokenKind};
use teal::parser::parse;
use teal::tli;

#[test]
fn lexer_inserts_semicolons_after_terminating_tokens() {
    let tokens = lex("let x = 1\nlet y = 2").unwrap();
    assert!(tokens.iter().any(|t| t.kind == TokenKind::Semi));
}

#[test]
fn parses_design_style_module() {
    let source = r#"
package http.cache
import std.io

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
    return "shape";
}
"#;
    let module = parse(source).unwrap();
    assert_eq!(module.package.as_deref(), Some("http.cache"));
    assert_eq!(module.items.len(), 3);
}

#[test]
fn tli_round_trips_deterministically() {
    let source = "package demo\nstruct Point { x: i32, y: i32 }\n";
    let module = parse(source).unwrap();
    let summary = tli::emit(&module, source);
    let encoded = tli::encode(&summary).unwrap();
    let decoded = tli::decode(&encoded).unwrap();
    assert_eq!(summary, decoded);
    assert_eq!(tli::encode(&summary).unwrap(), encoded);
}

#[test]
fn loader_checks_and_builds_tl_files() {
    let dir = std::env::temp_dir().join(format!("teal-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("main.tl"),
        "package demo\nstruct Point { x: i32, y: i32 }\npub fn ok() -> i32 { return 1 }\n",
    )
    .unwrap();

    assert!(teal::loader::check_path(&dir).is_ok());
    assert!(teal::loader::build_path(&dir).is_ok());
    assert!(dir.join("target").join("teal").join("demo.tli").exists());
    let _ = fs::remove_dir_all(&dir);
}
