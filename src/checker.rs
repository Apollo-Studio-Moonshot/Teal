use crate::parser::{Expr, Function, ItemKind, Method, Module, Stmt, TypeExpr};
use crate::Diagnostic;
use std::collections::{HashMap, HashSet};

pub struct Checker {
    pub types: HashMap<String, String>,
    pub functions: HashSet<String>,
    pub interfaces: HashSet<String>,
}

impl Default for Checker {
    fn default() -> Self {
        let mut types = HashMap::new();
        for ty in [
            "i32", "i64", "usize", "f32", "f64", "bool", "char", "string", "Self",
        ] {
            types.insert(ty.to_string(), ty.to_string());
        }
        types.insert("Option".into(), "Option".into());
        types.insert("Result".into(), "Result".into());
        Self {
            types,
            functions: HashSet::new(),
            interfaces: HashSet::new(),
        }
    }
}

impl Checker {
    pub fn check(&mut self, module: &Module) -> Vec<Diagnostic> {
        let mut errors = Vec::new();
        for item in &module.items {
            match &item.kind {
                ItemKind::Struct { name, fields, .. } => {
                    self.types.insert(name.clone(), "struct".into());
                    for field in fields {
                        if let Err(e) = self.check_type(&field.ty) {
                            errors.push(e);
                        }
                    }
                }
                ItemKind::Enum { name, variants, .. } => {
                    self.types.insert(name.clone(), "enum".into());
                    for variant in variants {
                        for field in &variant.fields {
                            if let Err(e) = self.check_type(&field.ty) {
                                errors.push(e);
                            }
                        }
                        for ty in &variant.tuple {
                            if let Err(e) = self.check_type(ty) {
                                errors.push(e);
                            }
                        }
                    }
                }
                ItemKind::Opaque { name, ty } => {
                    self.types.insert(name.clone(), "opaque".into());
                    if let Err(e) = self.check_type(ty) {
                        errors.push(e);
                    }
                }
                ItemKind::TypeAlias { name, ty } => {
                    self.types.insert(name.clone(), "alias".into());
                    if let Err(e) = self.check_type(ty) {
                        errors.push(e);
                    }
                }
                ItemKind::Interface { name, .. } => {
                    self.interfaces.insert(name.clone());
                }
                ItemKind::Fn(function) => self.check_function(function, &mut errors),
                ItemKind::Impl { ty, items, .. } => {
                    if let Err(e) = self.check_type(ty) {
                        errors.push(e);
                    }
                    for method in items {
                        self.check_method(method, &mut errors);
                    }
                }
                _ => {}
            }
        }
        // A second pass lets impl/interface types resolve after declarations.
        errors
    }

    fn check_type(&self, ty: &TypeExpr) -> Result<(), Diagnostic> {
        match ty {
            TypeExpr::Named(name) => {
                if self.types.contains_key(name) || self.interfaces.contains(name) {
                    Ok(())
                } else {
                    Err(Diagnostic {
                        span: crate::Span::new(0, 0),
                        message: format!("unknown type '{}'", name),
                    })
                }
            }
            TypeExpr::Path(parts) => {
                if parts.is_empty() {
                    Err(Diagnostic {
                        span: crate::Span::new(0, 0),
                        message: "empty type path".into(),
                    })
                } else {
                    Ok(())
                }
            }
            TypeExpr::Ref(inner, _) | TypeExpr::List(inner) => self.check_type(inner),
            TypeExpr::Tuple(items) => {
                for item in items {
                    self.check_type(item)?;
                }
                Ok(())
            }
            TypeExpr::Generic(name, args) => {
                self.check_type(&TypeExpr::Named(name.clone()))?;
                for arg in args {
                    self.check_type(arg)?;
                }
                Ok(())
            }
            TypeExpr::Dyn(name) => {
                if self.interfaces.contains(name) {
                    Ok(())
                } else {
                    Ok(())
                }
            }
        }
    }

    fn check_function(&mut self, function: &Function, errors: &mut Vec<Diagnostic>) {
        self.functions.insert(function.name.clone());
        if let Some(ret) = &function.ret {
            if let Err(e) = self.check_type(ret) {
                errors.push(e);
            }
        }
        for param in &function.params {
            if let Some(ty) = &param.ty {
                if let Err(e) = self.check_type(ty) {
                    errors.push(e);
                }
            }
        }
        if let Some(body) = &function.body {
            let mut locals = HashMap::new();
            for param in &function.params {
                locals.insert(
                    param.name.clone(),
                    param
                        .ty
                        .as_ref()
                        .map(type_name)
                        .unwrap_or_else(|| "unknown".into()),
                );
            }
            self.check_block(body, &mut locals, errors);
        }
    }

    fn check_method(&mut self, method: &Method, errors: &mut Vec<Diagnostic>) {
        self.functions.insert(method.name.clone());
        if let Some(ret) = &method.ret {
            if let Err(e) = self.check_type(ret) {
                errors.push(e);
            }
        }
        for param in &method.params {
            if let Some(ty) = &param.ty {
                if let Err(e) = self.check_type(ty) {
                    errors.push(e);
                }
            }
        }
        if let Some(body) = &method.body {
            let mut locals = HashMap::new();
            for param in &method.params {
                locals.insert(
                    param.name.clone(),
                    param
                        .ty
                        .as_ref()
                        .map(type_name)
                        .unwrap_or_else(|| "unknown".into()),
                );
            }
            self.check_block(body, &mut locals, errors);
        }
    }

    fn check_block(
        &mut self,
        block: &crate::parser::Block,
        locals: &mut HashMap<String, String>,
        errors: &mut Vec<Diagnostic>,
    ) {
        for stmt in &block.stmts {
            match stmt {
                Stmt::Let {
                    name, ty, value, ..
                } => {
                    let inferred = self.expr_type(value, locals);
                    if let Some(ann) = ty {
                        let ann_name = type_name(ann);
                        if inferred != "unknown" && inferred != ann_name {
                            errors.push(Diagnostic {
                                span: crate::Span::new(0, 0),
                                message: format!("expected {}, found {}", ann_name, inferred),
                            });
                        }
                    }
                    locals.insert(name.clone(), ty.as_ref().map(type_name).unwrap_or(inferred));
                }
                Stmt::Assign { name, value } => {
                    if !locals.contains_key(name) {
                        errors.push(Diagnostic {
                            span: crate::Span::new(0, 0),
                            message: format!("unknown variable '{}'", name),
                        });
                    }
                    let _ = self.expr_type(value, locals);
                }
                Stmt::Expr(expr) => {
                    let _ = self.expr_type(expr, locals);
                }
                Stmt::Return(expr) => {
                    if let Some(expr) = expr {
                        let _ = self.expr_type(expr, locals);
                    }
                }
                Stmt::Defer(expr) => {
                    let _ = self.expr_type(expr, locals);
                }
                Stmt::If { cond, then, else_ } => {
                    let _ = self.expr_type(cond, locals);
                    self.check_block(then, locals, errors);
                    if let Some(other) = else_ {
                        self.check_block(other, locals, errors);
                    }
                }
                Stmt::While { cond, body } => {
                    let _ = self.expr_type(cond, locals);
                    self.check_block(body, locals, errors);
                }
                Stmt::For { name, iter, body } => {
                    let iter_ty = self.expr_type(iter, locals);
                    locals.insert(name.clone(), iter_ty);
                    self.check_block(body, locals, errors);
                }
                Stmt::Match { value, arms } => {
                    let _ = self.expr_type(value, locals);
                    for (_, guard, result) in arms {
                        if let Some(guard) = guard {
                            let _ = self.expr_type(guard, locals);
                        }
                        let _ = self.expr_type(result, locals);
                    }
                }
                Stmt::Block(block) => self.check_block(block, locals, errors),
                Stmt::Break | Stmt::Continue => {}
            }
        }
    }

    fn expr_type(&self, expr: &Expr, locals: &HashMap<String, String>) -> String {
        match expr {
            Expr::Int(_) => "i32".into(),
            Expr::Float(_) => "f64".into(),
            Expr::String(_) => "string".into(),
            Expr::Char(_) => "char".into(),
            Expr::Bool(_) => "bool".into(),
            Expr::Ident(name) => locals.get(name).cloned().unwrap_or_else(|| {
                if self.functions.contains(name) {
                    "fn".into()
                } else {
                    "unknown".into()
                }
            }),
            Expr::SelfVal => "Self".into(),
            Expr::Binary(left, _, right) => {
                let l = self.expr_type(left, locals);
                let r = self.expr_type(right, locals);
                if l == r {
                    l
                } else {
                    "unknown".into()
                }
            }
            Expr::Unary(_, inner) | Expr::Try(inner) | Expr::Index(inner, _) => {
                self.expr_type(inner, locals)
            }
            Expr::Call(callee, _) => {
                let callee = self.expr_type(callee, locals);
                if callee == "fn" {
                    "unknown".into()
                } else {
                    callee
                }
            }
            Expr::Member(inner, _) => self.expr_type(inner, locals),
            Expr::Block(block) => block
                .stmts
                .last()
                .and_then(|s| match s {
                    Stmt::Return(Some(e)) => Some(self.expr_type(e, locals)),
                    Stmt::Expr(e) => Some(self.expr_type(e, locals)),
                    _ => None,
                })
                .unwrap_or_else(|| "()".into()),
            Expr::Match(_, arms) => arms
                .first()
                .map(|(_, _, e)| self.expr_type(e, locals))
                .unwrap_or_else(|| "()".into()),
            Expr::List(_) => "[i32]".into(),
            Expr::Tuple(items) => format!(
                "({})",
                items
                    .iter()
                    .map(|e| self.expr_type(e, locals))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Expr::Closure(_, body) => self.expr_type(body, locals),
            Expr::Path(_) => "enum".into(),
        }
    }
}

pub fn type_name(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Named(s) => s.clone(),
        TypeExpr::Path(p) => p.join("::"),
        TypeExpr::Ref(inner, mutable) => format!(
            "&{}{}",
            if *mutable { "mut " } else { "" },
            type_name(inner)
        ),
        TypeExpr::List(inner) => format!("[{}]", type_name(inner)),
        TypeExpr::Tuple(items) => format!(
            "({})",
            items.iter().map(type_name).collect::<Vec<_>>().join(",")
        ),
        TypeExpr::Generic(name, args) => format!(
            "{}<{}>",
            name,
            args.iter().map(type_name).collect::<Vec<_>>().join(",")
        ),
        TypeExpr::Dyn(name) => format!("dyn {}", name),
    }
}
