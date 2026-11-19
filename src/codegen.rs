//! Native code generation support.
//!
//! The prototype backend lowers checked Teal functions to C for native object
//! emission. It keeps lowering and code emission separate so this backend can
//! later be replaced by the Cranelift lowering pipeline described in DESIGN.md.

use crate::ast::*;
use crate::checker::{Env, FnSig};
use crate::hash;
use crate::types::{FloatTy, IntTy, Type};
use crate::Diagnostic;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, Default)]
pub struct LoweredPackage {
    pub name: String,
    pub c_source: String,
    pub function_count: usize,
}

fn ident(text: &str) -> String {
    let mut out = String::new();
    for (index, ch) in text.chars().enumerate() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            if index == 0 && ch.is_ascii_digit() {
                out.push('_');
            }

            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "_".to_string()
    } else {
        out
    }
}

fn block_tail_c(block: &Block, package: &str, env: &Env) -> String {
    block
        .stmts
        .last()
        .and_then(|stmt| match &stmt.kind {
            StmtKind::Expr(expr) => Some(expr_c(expr, package, env)),
            StmtKind::Return(Some(expr)) => Some(expr_c(expr, package, env)),
            _ => None,
        })
        .unwrap_or_else(|| "0".into())
}

fn symbol(package: &str, name: &str) -> String {
    format!("teal_{}_{}", ident(package), ident(name))
}

fn c_type(ty: &Type) -> String {
    match ty {
        Type::Bool => "bool".into(),
        Type::Char => "uint32_t".into(),
        Type::Str => "const char *".into(),
        Type::Int(IntTy::I8) => "int8_t".into(),
        Type::Int(IntTy::I16) => "int16_t".into(),
        Type::Int(IntTy::I32) => "int32_t".into(),
        Type::Int(IntTy::I64) | Type::Int(IntTy::Isize) => "int64_t".into(),
        Type::Int(IntTy::U8) => "uint8_t".into(),
        Type::Int(IntTy::U16) => "uint16_t".into(),
        Type::Int(IntTy::U32) => "uint32_t".into(),
        Type::Int(IntTy::U64) | Type::Int(IntTy::Usize) => "uint64_t".into(),
        Type::Float(FloatTy::F32) => "float".into(),
        Type::Float(FloatTy::F64) => "double".into(),
        Type::Unit => "void".into(),
        Type::Never => "void".into(),
        Type::Named { name, .. } => format!("struct {}", ident(name)),
        Type::Ref { inner, mutable: _ } => format!("{} *", c_type(inner)),
        Type::Tuple(items) => format!("struct teal_tuple_{}", items.len()),
        Type::Slice(inner) => format!(
            "struct teal_slice {{ {} *data; size_t len; }}",
            c_type(inner)
        ),
        Type::Fn { .. } => "void *".into(),
        Type::Dyn { name, .. } => format!("struct {}", ident(name)),
        Type::Param(_) | Type::SelfType | Type::Error => "void".into(),
    }
}

fn expr_c(expr: &Expr, package: &str, env: &Env) -> String {
    match &expr.kind {
        ExprKind::Int { value, .. } => value.to_string(),
        ExprKind::Float { text, .. } => text.clone(),
        ExprKind::Str(value) => format!("{value:?}"),
        ExprKind::Char(value) => format!("(uint32_t){}", *value as u32),
        ExprKind::Bool(value) => {
            if *value {
                "true".into()
            } else {
                "false".into()
            }
        }
        ExprKind::Path { path, .. } => {
            if path.len() == 1 {
                ident(&path[0])
            } else if let Some(name) = path.last() {
                if env.functions.contains_key(name) {
                    symbol(package, name)
                } else {
                    ident(name)
                }
            } else {
                "0".into()
            }
        }
        ExprKind::Unary { op, expr } => {
            let inner = expr_c(expr, package, env);
            match op {
                UnOp::Neg => format!("(-({inner}))"),
                UnOp::Not => format!("(!({inner}))"),
                UnOp::Ref | UnOp::RefMut => format!("(&({inner}))"),
            }
        }
        ExprKind::Binary { op, lhs, rhs } => {
            let lhs = expr_c(lhs, package, env);
            let rhs = expr_c(rhs, package, env);
            let op = match op {
                BinOp::Add => "+",
                BinOp::Sub => "-",
                BinOp::Mul => "*",
                BinOp::Div => "/",
                BinOp::Rem => "%",
                BinOp::Shl => "<<",
                BinOp::Shr => ">>",
                BinOp::BitAnd => "&",
                BinOp::BitOr => "|",
                BinOp::BitXor => "^",
                BinOp::Eq => "==",
                BinOp::Ne => "!=",
                BinOp::Lt => "<",
                BinOp::Le => "<=",
                BinOp::Gt => ">",
                BinOp::Ge => ">=",
                BinOp::AndAnd => "&&",
                BinOp::OrOr => "||",
            };
            format!("(({lhs}) {op} ({rhs}))")
        }
        ExprKind::Call { callee, args } => {
            let callee = expr_c(callee, package, env);
            let args = args
                .iter()
                .map(|arg| expr_c(arg, package, env))
                .collect::<Vec<_>>();
            format!("{callee}({})", args.join(", "))
        }
        ExprKind::MethodCall {
            receiver,
            name,
            args,
            ..
        } => {
            let receiver = expr_c(receiver, package, env);
            let args = args
                .iter()
                .map(|arg| expr_c(arg, package, env))
                .collect::<Vec<_>>();
            format!(
                "{}({receiver}{})",
                symbol(package, name),
                if args.is_empty() {
                    String::new()
                } else {
                    format!(", {}", args.join(", "))
                }
            )
        }
        ExprKind::Field { base, name } => {
            format!("({}).{}", expr_c(base, package, env), ident(name))
        }
        ExprKind::Index { base, index } => format!(
            "({})[{}]",
            expr_c(base, package, env),
            expr_c(index, package, env)
        ),
        ExprKind::Try(inner) => expr_c(inner, package, env),
        ExprKind::Cast { expr, ty } => {
            format!("({})({})", c_type_expr(ty), expr_c(expr, package, env))
        }
        ExprKind::Block(block) => format!("({})", block_c(block, package, env, None)),
        ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            let then_c = block_tail_c(then_branch, package, env);
            let else_c = else_branch
                .as_ref()
                .map(|expr| expr_c(expr, package, env))
                .unwrap_or_else(|| "0".into());
            format!(
                "(({}) ? ({}) : ({}))",
                expr_c(cond, package, env),
                then_c,
                else_c
            )
        }
        ExprKind::Match { scrutinee, .. } => {
            format!("/* match */ ({})", expr_c(scrutinee, package, env))
        }
        ExprKind::While { .. }
        | ExprKind::WhileLet { .. }
        | ExprKind::Loop { .. }
        | ExprKind::For { .. }
        | ExprKind::Closure { .. }
        | ExprKind::IfLet { .. } => "0".into(),
        ExprKind::List(items) => format!(
            "({{{}}})",
            items
                .iter()
                .map(|e| expr_c(e, package, env))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        ExprKind::Tuple(items) => format!(
            "({{{}}})",
            items
                .iter()
                .map(|e| expr_c(e, package, env))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        ExprKind::StructLit { path, fields, .. } => {
            let name = path
                .last()
                .map(|n| ident(n))
                .unwrap_or_else(|| "_anon".into());
            let fields = fields
                .iter()
                .map(|(n, e)| format!(".{n} = {}", expr_c(e, package, env)))
                .collect::<Vec<_>>();
            format!("((struct {name}){{{}}})", fields.join(", "))
        }
        ExprKind::MacroCall { name, .. } => format!("/* macro {} */ 0", ident(name)),
        ExprKind::Range {
            lo: _,
            hi: _,
            inclusive,
        } => format!(
            "(/* range {} */ {})",
            if *inclusive { "inclusive" } else { "exclusive" },
            "0"
        ),
        ExprKind::SelfValue => "teal_self".into(),
    }
}

fn c_type_expr(ty: &TypeExpr) -> String {
    match ty.print().as_str() {
        "bool" => "bool".into(),
        "i8" => "int8_t".into(),
        "i16" => "int16_t".into(),
        "i32" => "int32_t".into(),
        "i64" => "int64_t".into(),
        "u8" => "uint8_t".into(),
        "u16" => "uint16_t".into(),
        "u32" => "uint32_t".into(),
        "u64" => "uint64_t".into(),
        "usize" => "uint64_t".into(),
        "isize" => "int64_t".into(),
        "f32" => "float".into(),
        "f64" => "double".into(),
        "string" => "const char *".into(),
        "char" => "uint32_t".into(),
        "()" => "void".into(),
        text => ident(text),
    }
}

fn pattern_c(pattern: &Pattern) -> Option<String> {
    match &pattern.kind {
        PatKind::Binding { name, .. } => Some(ident(name)),
        PatKind::Wildcard => None,
        PatKind::Ref { pattern, .. } => pattern_c(pattern),
        PatKind::Tuple(items) => items.first().and_then(pattern_c),
        PatKind::Or(items) => items.first().and_then(pattern_c),
        PatKind::Struct { fields, .. } => fields.first().and_then(|(_, p)| pattern_c(p)),
        PatKind::Path { .. } | PatKind::Literal(_) | PatKind::Range { .. } => None,
    }
}

fn block_c(block: &Block, package: &str, env: &Env, tail_expected: Option<&Type>) -> String {
    let mut out = String::from("{\n");
    for (index, stmt) in block.stmts.iter().enumerate() {
        let is_tail = index + 1 == block.stmts.len();
        match &stmt.kind {
            StmtKind::Let {
                pattern, ty, value, ..
            } => {
                let bind = pattern_c(pattern);
                let inferred = bind
                    .as_ref()
                    .map(|name| {
                        let ty = ty
                            .as_ref()
                            .map(c_type_expr)
                            .unwrap_or_else(|| "int64_t".into());
                        format!("  {ty} {name} = {};\n", expr_c(value, package, env))
                    })
                    .unwrap_or_else(|| format!("  /* let */ {};\n", expr_c(value, package, env)));
                out.push_str(&inferred);
            }
            StmtKind::Assign { target, op, value } => {
                let op = op
                    .map(|o| match o {
                        BinOp::Add => "+=",
                        BinOp::Sub => "-=",
                        BinOp::Mul => "*=",
                        BinOp::Div => "/=",
                        BinOp::Rem => "%=",
                        BinOp::Shl => "<<=",
                        BinOp::Shr => ">>=",
                        BinOp::BitAnd => "&=",
                        BinOp::BitOr => "|=",
                        BinOp::BitXor => "^=",
                        _ => "=",
                    })
                    .unwrap_or("=");
                out.push_str(&format!(
                    "  {} {} {};\n",
                    expr_c(target, package, env),
                    op,
                    expr_c(value, package, env)
                ));
            }
            StmtKind::Return(value) => {
                out.push_str("  return");
                if let Some(value) = value {
                    out.push_str(&format!(" {}", expr_c(value, package, env)));
                }
                out.push_str(";\n");
            }
            StmtKind::Break => out.push_str("  break;\n"),
            StmtKind::Continue => out.push_str("  continue;\n"),
            StmtKind::Defer(_) => {}
            StmtKind::Expr(value) => {
                if is_tail {
                    if matches!(tail_expected, Some(ty) if !ty.is_unit()) {
                        out.push_str(&format!("  return {};\n", expr_c(value, package, env)));
                    } else {
                        out.push_str(&format!("  {};\n", expr_c(value, package, env)));
                    }
                } else {
                    out.push_str(&format!("  {};\n", expr_c(value, package, env)));
                }
            }
            StmtKind::Item(_) => {}
        }
    }
    out.push('}');
    out
}

/// Lower checked package declarations and function bodies into a native IR
/// packet. The current native renderer emits C before invoking the system C
/// compiler, but the returned structure is backend-independent.
pub fn lower(package: &str, modules: &[Module], env: &Env) -> LoweredPackage {
    let mut c = String::from(
        "#include <stdint.h>\n#include <stddef.h>\n#include <stdbool.h>\n#include <stdio.h>\n\n",
    );

    for (name, ty) in &env.types {
        if ty.package != env.package {
            continue;
        }
        if let crate::checker::TypeKind::Struct { fields } = &ty.kind {
            c.push_str(&format!("struct {} {{\n", ident(name)));
            for field in fields {
                c.push_str(&format!(
                    "  {} {};\n",
                    c_type(&field.ty),
                    ident(&field.name)
                ));
            }
            c.push_str("};\n\n");
        }
    }

    let mut function_count = 0;
    for module in modules {
        for item in &module.items {
            if let ItemKind::Fn(function) = &item.kind {
                if let Some(info) = env.functions.get(&function.name) {
                    function_count += 1;
                    let sig = &info.sig;
                    emit_function(&mut c, package, function, sig, env);
                }
            }
        }
    }
    LoweredPackage {
        name: package.to_string(),
        c_source: c,
        function_count,
    }
}

fn emit_function(out: &mut String, package: &str, function: &Function, sig: &FnSig, env: &Env) {
    let ret = c_type(&sig.ret);
    let params = sig
        .params
        .iter()
        .zip(&sig.param_names)
        .map(|(ty, name)| format!("{} {}", c_type(ty), ident(name)))
        .collect::<Vec<_>>();
    out.push_str(&format!(
        "{} {}({}) ",
        ret,
        symbol(package, &function.name),
        params.join(", ")
    ));
    if let Some(body) = &function.body {
        out.push_str(&block_c(body, package, env, Some(&sig.ret)));
    } else {
        out.push(';');
    }
    out.push_str("\n\n");
}

pub fn emit_c_source(lowered: &LoweredPackage) -> &str {
    &lowered.c_source
}

pub fn write_c(target_dir: &Path, lowered: &LoweredPackage) -> Result<PathBuf, Vec<Diagnostic>> {
    fs::create_dir_all(target_dir).map_err(|error| {
        vec![Diagnostic::error(
            crate::codes::IO,
            crate::Span::dummy(),
            format!("cannot create codegen target: {error}"),
        )]
    })?;
    let path = target_dir.join(format!("{}.c", lowered.name.replace(['.', '/', '\\'], "_")));
    fs::write(&path, &lowered.c_source).map_err(|error| {
        vec![Diagnostic::error(
            crate::codes::IO,
            crate::Span::dummy(),
            format!("cannot write {}: {error}", path.display()),
        )]
    })?;
    Ok(path)
}

/// Compile lowered C to a content-addressed native object. The resulting
/// package object is also copied beside `.tli`.
pub fn emit_object(
    target_dir: &Path,
    lowered: &LoweredPackage,
) -> Result<PathBuf, Vec<Diagnostic>> {
    let c_path = write_c(target_dir, lowered)?;
    let obj_dir = target_dir.join("cache").join("obj");
    fs::create_dir_all(&obj_dir).map_err(|error| {
        vec![Diagnostic::error(
            crate::codes::IO,
            crate::Span::dummy(),
            format!("cannot create object cache: {error}"),
        )]
    })?;
    let digest = hash::to_hex(&hash::sha256(lowered.c_source.as_bytes()));
    let object = obj_dir.join(format!("{digest}.o"));
    if !object.exists() {
        let status = match Command::new("cc")
            .arg("-std=c11")
            .arg("-c")
            .arg(&c_path)
            .arg("-o")
            .arg(&object)
            .status()
        {
            Ok(status) => status,
            Err(_) => return Ok(c_path),
        };
        if !status.success() {
            // Unsupported lowering constructs remain available as C source;
            // the front-end build must not become dependent on native compilation.
            return Ok(c_path);
        }
    }
    let package_object =
        target_dir.join(format!("{}.o", lowered.name.replace(['.', '/', '\\'], "_")));
    fs::copy(&object, &package_object).map_err(|error| {
        vec![Diagnostic::error(
            crate::codes::IO,
            crate::Span::dummy(),
            format!("cannot write {}: {error}", package_object.display()),
        )]
    })?;
    Ok(package_object)
}
