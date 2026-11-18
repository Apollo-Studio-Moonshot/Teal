//! Per-function bidirectional type checking.
//!
//! There is no inference state that escapes a function body (DESIGN.md §1,
//! §4.1). Each body is checked independently against fully annotated
//! signatures, and impl/conformance lookups are hash-table operations against
//! the package summary, never searches through the impl graph.
//!
//! The checker also decides match exhaustiveness over closed enums and builds
//! the environment that `tli` serializes for downstream packages.

use crate::ast::*;
use crate::tli::Tli;
use crate::types::{parse_type, IntTy, Type};
use crate::{codes, Diagnostic, Span};
use std::collections::{BTreeMap, BTreeSet};

// ---------------------------------------------------------------- environment

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundInfo {
    pub iface: String,
    pub args: Vec<Type>,
    pub assoc: BTreeMap<String, Type>,
    pub fn_sig: Option<(Vec<Type>, Type)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenericInfo {
    pub name: String,
    pub bounds: Vec<BoundInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FnSig {
    pub name: String,
    pub generics: Vec<GenericInfo>,
    pub params: Vec<Type>,
    pub param_names: Vec<String>,
    pub ret: Type,
}

impl FnSig {
    pub fn has_self(&self) -> bool {
        self.param_names.first().is_some_and(|n| n == "self")
    }

    pub fn method_params(&self) -> &[Type] {
        if self.has_self() {
            &self.params[1..]
        } else {
            &self.params
        }
    }

    pub fn method_param_names(&self) -> &[String] {
        if self.has_self() {
            &self.param_names[1..]
        } else {
            &self.param_names
        }
    }

    pub fn print(&self) -> String {
        format!(
            "fn {}{}({}) -> {}",
            self.name,
            if self.generics.is_empty() {
                String::new()
            } else {
                format!(
                    "<{}>",
                    self.generics
                        .iter()
                        .map(|g| g.name.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            },
            self.param_names
                .iter()
                .zip(&self.params)
                .map(|(n, t)| format!("{n}: {t}"))
                .collect::<Vec<_>>()
                .join(", "),
            self.ret
        )
    }
}

#[derive(Clone, Debug)]
pub struct FieldInfo {
    pub name: String,
    pub ty: Type,
}

#[derive(Clone, Debug)]
pub struct VariantInfo {
    pub name: String,
    pub fields: Vec<FieldInfo>,
    pub tuple: Vec<Type>,
    pub span: Span,
}

impl VariantInfo {
    pub fn is_unit(&self) -> bool {
        self.fields.is_empty() && self.tuple.is_empty()
    }
}

#[derive(Clone, Debug)]
pub enum TypeKind {
    Struct {
        fields: Vec<FieldInfo>,
    },
    Enum {
        variants: Vec<VariantInfo>,
        open: bool,
    },
    Opaque {
        repr: Type,
    },
    Alias {
        target: Type,
    },
}

#[derive(Clone, Debug)]
pub struct TypeInfo {
    pub name: String,
    pub package: String,
    pub public: bool,
    pub generics: Vec<String>,
    pub kind: TypeKind,
    pub docs: Vec<String>,
    pub span: Span,
}

impl TypeInfo {
    pub fn param_types(&self) -> Vec<Type> {
        self.generics.iter().cloned().map(Type::Param).collect()
    }

    pub fn as_type(&self) -> Type {
        Type::Named {
            name: self.name.clone(),
            args: self.param_types(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct MethodInfo {
    pub sig: FnSig,
    pub body: Option<Block>,
    pub span: Span,
    pub docs: Vec<String>,
    /// Set when the method is inherited from an interface's provided method.
    pub from_interface: Option<String>,
}

#[derive(Clone, Debug)]
pub struct InterfaceInfo {
    pub name: String,
    pub package: String,
    pub public: bool,
    pub generics: Vec<GenericInfo>,
    pub assoc_types: Vec<String>,
    pub methods: BTreeMap<String, MethodInfo>,
    pub span: Span,
    pub docs: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ImplInfo {
    pub package: String,
    pub generics: Vec<GenericInfo>,
    pub self_ty: Type,
    pub iface: Option<String>,
    pub iface_args: Vec<Type>,
    pub assoc: BTreeMap<String, Type>,
    pub methods: BTreeMap<String, MethodInfo>,
    pub span: Span,
    pub public: bool,
}

impl ImplInfo {
    pub fn print(&self) -> String {
        match &self.iface {
            Some(iface) => format!("impl {iface} for {}", self.self_ty),
            None => format!("impl {}", self.self_ty),
        }
    }
}

#[derive(Clone, Debug)]
pub struct FunctionInfo {
    pub sig: FnSig,
    pub package: String,
    pub public: bool,
    pub docs: Vec<String>,
    pub span: Span,
}

#[derive(Clone, Debug, Default)]
pub struct Env {
    pub package: String,
    pub types: BTreeMap<String, TypeInfo>,
    pub interfaces: BTreeMap<String, InterfaceInfo>,
    pub functions: BTreeMap<String, FunctionInfo>,
    pub consts: BTreeMap<String, Type>,
    /// Package that defines each constant (used to avoid re-exporting deps).
    pub const_packages: BTreeMap<String, String>,
    pub impls: Vec<ImplInfo>,
    pub macros: BTreeMap<String, crate::macros::MacroDef>,
    /// Packages whose symbols are visible in this build. Purely informational.
    pub dependencies: BTreeSet<String>,
}

pub struct CheckResult {
    pub env: Env,
    pub diagnostics: Vec<Diagnostic>,
    pub functions_checked: usize,
    pub types_checked: usize,
}

/// Check all modules of one package. `external` are dependency summaries.
pub fn check_package(package: &str, modules: &[Module], external: &[Tli]) -> CheckResult {
    let mut env = Env::builtin();
    env.package = package.to_string();
    for tli in external {
        env.import_tli(tli);
    }

    let mut diagnostics = Vec::new();
    let mut declared_types = BTreeSet::new();
    let mut declared_interfaces = BTreeSet::new();

    // Phase 1: declare names so signatures may refer forward.
    for module in modules {
        for item in &module.items {
            match &item.kind {
                ItemKind::Struct { name, .. }
                | ItemKind::Enum { name, .. }
                | ItemKind::Opaque { name, .. }
                | ItemKind::TypeAlias { name, .. } => {
                    if !declared_types.insert(name.clone()) {
                        diagnostics.push(Diagnostic::error(
                            codes::DUPLICATE_DEF,
                            item.span,
                            format!(
                                "type `{name}` is defined more than once in package `{package}`"
                            ),
                        ));
                    }
                    env.types.entry(name.clone()).or_insert_with(|| TypeInfo {
                        name: name.clone(),
                        package: package.to_string(),
                        public: item.vis == Visibility::Public,
                        generics: Vec::new(),
                        kind: TypeKind::Opaque { repr: Type::Unit },
                        docs: item.docs.clone(),
                        span: item.span,
                    });
                }
                ItemKind::Interface { name, .. } => {
                    declared_interfaces.insert(name.clone());
                }
                _ => {}
            }
        }
    }

    // Phase 2: elaborate declarations and signatures.
    let mut impl_spans: BTreeMap<Span, usize> = BTreeMap::new();
    let mut body_work: Vec<BodyWork> = Vec::new();

    for module in modules {
        for item in &module.items {
            match &item.kind {
                ItemKind::Struct {
                    name,
                    generics,
                    fields,
                } => {
                    let ctx = ElabCtx {
                        generics: generics.names().into_iter().collect(),
                        ..Default::default()
                    };
                    let mut elaborated = Vec::new();
                    for field in fields {
                        match env.elaborate(&field.ty, &ctx) {
                            Ok(ty) => elaborated.push(FieldInfo {
                                name: field.name.clone(),
                                ty,
                            }),
                            Err(diag) => diagnostics.push(diag),
                        }
                    }
                    env.types.insert(
                        name.clone(),
                        TypeInfo {
                            name: name.clone(),
                            package: package.to_string(),
                            public: item.vis == Visibility::Public,
                            generics: generics.names(),
                            kind: TypeKind::Struct { fields: elaborated },
                            docs: item.docs.clone(),
                            span: item.span,
                        },
                    );
                }
                ItemKind::Enum {
                    name,
                    generics,
                    open,
                    variants,
                } => {
                    let ctx = ElabCtx {
                        generics: generics.names().into_iter().collect(),
                        ..Default::default()
                    };
                    let mut elaborated = Vec::new();
                    for variant in variants {
                        let mut fields = Vec::new();
                        for field in &variant.fields {
                            match env.elaborate(&field.ty, &ctx) {
                                Ok(ty) => fields.push(FieldInfo {
                                    name: field.name.clone(),
                                    ty,
                                }),
                                Err(diag) => diagnostics.push(diag),
                            }
                        }
                        let mut tuple = Vec::new();
                        for ty in &variant.tuple {
                            match env.elaborate(ty, &ctx) {
                                Ok(ty) => tuple.push(ty),
                                Err(diag) => diagnostics.push(diag),
                            }
                        }
                        elaborated.push(VariantInfo {
                            name: variant.name.clone(),
                            fields,
                            tuple,
                            span: variant.span,
                        });
                    }
                    env.types.insert(
                        name.clone(),
                        TypeInfo {
                            name: name.clone(),
                            package: package.to_string(),
                            public: item.vis == Visibility::Public,
                            generics: generics.names(),
                            kind: TypeKind::Enum {
                                variants: elaborated,
                                open: *open,
                            },
                            docs: item.docs.clone(),
                            span: item.span,
                        },
                    );
                }
                ItemKind::Opaque { name, ty } => {
                    let ctx = ElabCtx::default();
                    let repr = env.elaborate(ty, &ctx).unwrap_or(Type::Error);
                    env.types.insert(
                        name.clone(),
                        TypeInfo {
                            name: name.clone(),
                            package: package.to_string(),
                            public: item.vis == Visibility::Public,
                            generics: Vec::new(),
                            kind: TypeKind::Opaque { repr },
                            docs: item.docs.clone(),
                            span: item.span,
                        },
                    );
                }
                ItemKind::TypeAlias { name, generics, ty } => {
                    if !generics.is_empty() {
                        diagnostics.push(
                            Diagnostic::error(
                                codes::UNKNOWN_TYPE,
                                item.span,
                                "generic type aliases are not supported",
                            )
                            .with_note("DESIGN.md §3.5: no generic type aliases, no GATs"),
                        );
                    }
                    let ctx = ElabCtx {
                        generics: generics.names().into_iter().collect(),
                        ..Default::default()
                    };
                    let target = env.elaborate(ty, &ctx).unwrap_or(Type::Error);
                    env.types.insert(
                        name.clone(),
                        TypeInfo {
                            name: name.clone(),
                            package: package.to_string(),
                            public: item.vis == Visibility::Public,
                            generics: generics.names(),
                            kind: TypeKind::Alias { target },
                            docs: item.docs.clone(),
                            span: item.span,
                        },
                    );
                }
                _ => {}
            }
        }
    }

    // Interfaces first, because impl conformance consults them.
    for module in modules {
        for item in &module.items {
            if let ItemKind::Interface {
                name,
                generics,
                assoc_types,
                methods,
            } = &item.kind
            {
                let mut generic_infos = Vec::new();
                let mut generic_names = BTreeSet::new();
                for param in &generics.params {
                    generic_names.insert(param.name.clone());
                    let mut bounds = Vec::new();
                    for bound in &param.bounds {
                        match env.elaborate_bound(
                            bound,
                            &ElabCtx {
                                generics: generic_names.clone(),
                                ..Default::default()
                            },
                        ) {
                            Ok(b) => bounds.push(b),
                            Err(diag) => diagnostics.push(diag),
                        }
                    }
                    generic_infos.push(GenericInfo {
                        name: param.name.clone(),
                        bounds,
                    });
                }
                let mut assoc_map = BTreeMap::new();
                for assoc in assoc_types {
                    assoc_map.insert(
                        assoc.name.clone(),
                        Type::named(format!("Self::{}", assoc.name)),
                    );
                }
                let ctx = ElabCtx {
                    generics: generic_names,
                    self_ty: Some(Type::SelfType),
                    assoc: assoc_map,
                };
                let mut method_map = BTreeMap::new();
                for method in methods {
                    match env.elaborate_method(method, &ctx, item.vis) {
                        Ok(info) => {
                            if let Some(body) = &info.body {
                                body_work.push(BodyWork::Interface {
                                    iface: name.clone(),
                                    method: method.clone(),
                                    sig: info.sig.clone(),
                                });
                                let _ = body;
                            }
                            if method_map.insert(method.name.clone(), info).is_some() {
                                diagnostics.push(Diagnostic::error(
                                    codes::DUPLICATE_DEF,
                                    method.span,
                                    format!(
                                        "method `{}` is declared twice in interface `{name}`",
                                        method.name
                                    ),
                                ));
                            }
                        }
                        Err(diag) => diagnostics.push(diag),
                    }
                }
                env.interfaces.insert(
                    name.clone(),
                    InterfaceInfo {
                        name: name.clone(),
                        package: package.to_string(),
                        public: item.vis == Visibility::Public,
                        generics: generic_infos,
                        assoc_types: assoc_types.iter().map(|a| a.name.clone()).collect(),
                        methods: method_map,
                        span: item.span,
                        docs: item.docs.clone(),
                    },
                );
            }
        }
    }

    // Impls and free functions.
    for module in modules {
        for item in &module.items {
            match &item.kind {
                ItemKind::Impl {
                    generics,
                    iface,
                    ty,
                    assoc,
                    methods,
                } => {
                    let mut generic_infos = Vec::new();
                    let mut generic_names = BTreeSet::new();
                    for param in &generics.params {
                        generic_names.insert(param.name.clone());
                        let mut bounds = Vec::new();
                        for bound in &param.bounds {
                            match env.elaborate_bound(
                                bound,
                                &ElabCtx {
                                    generics: generic_names.clone(),
                                    ..Default::default()
                                },
                            ) {
                                Ok(b) => bounds.push(b),
                                Err(diag) => diagnostics.push(diag),
                            }
                        }
                        generic_infos.push(GenericInfo {
                            name: param.name.clone(),
                            bounds,
                        });
                    }
                    let self_base_ctx = ElabCtx {
                        generics: generic_names.clone(),
                        ..Default::default()
                    };
                    let self_ty = match env.elaborate(ty, &self_base_ctx) {
                        Ok(ty) => ty,
                        Err(diag) => {
                            diagnostics.push(diag);
                            Type::Error
                        }
                    };

                    let (iface_name, iface_args) = match iface {
                        Some(iface_ty) => match &iface_ty.kind {
                            TypeExprKind::Path { path, args, .. } => {
                                let name = path.last().cloned().unwrap_or_default();
                                let mut elaborated = Vec::new();
                                for arg in args {
                                    match env.elaborate(arg, &self_base_ctx) {
                                        Ok(ty) => elaborated.push(ty),
                                        Err(diag) => diagnostics.push(diag),
                                    }
                                }
                                (Some(name), elaborated)
                            }
                            _ => {
                                diagnostics.push(Diagnostic::error(
                                    codes::INVALID_IMPL,
                                    iface_ty.span,
                                    "expected an interface name after `impl`",
                                ));
                                (None, Vec::new())
                            }
                        },
                        None => (None, Vec::new()),
                    };

                    let mut assoc_map = BTreeMap::new();
                    for binding in assoc {
                        match env.elaborate(&binding.ty, &self_base_ctx) {
                            Ok(ty) => {
                                assoc_map.insert(binding.name.clone(), ty);
                            }
                            Err(diag) => diagnostics.push(diag),
                        }
                    }

                    let ctx = ElabCtx {
                        generics: generic_names.clone(),
                        self_ty: Some(self_ty.clone()),
                        assoc: assoc_map.clone(),
                    };
                    let mut method_map = BTreeMap::new();
                    for method in methods {
                        match env.elaborate_method(method, &ctx, item.vis) {
                            Ok(info) => {
                                if method_map.insert(method.name.clone(), info).is_some() {
                                    diagnostics.push(Diagnostic::error(
                                        codes::DUPLICATE_DEF,
                                        method.span,
                                        format!("method `{}` is defined twice", method.name),
                                    ));
                                }
                            }
                            Err(diag) => diagnostics.push(diag),
                        }
                    }

                    // Interface conformance.
                    if let Some(iface_name) = &iface_name {
                        match env.interfaces.get(iface_name).cloned() {
                            None => diagnostics.push(Diagnostic::error(
                                codes::UNKNOWN_TYPE,
                                item.span,
                                format!("unknown interface `{iface_name}`"),
                            )),
                            Some(interface) => {
                                // Orphan rule: an impl must be declared in the
                                // package defining the type or the interface.
                                let type_package = type_package_of(&self_ty, &env);
                                if interface.package != package
                                    && type_package.as_deref() != Some(package)
                                {
                                    diagnostics.push(
                                        Diagnostic::error(
                                            codes::ORPHAN_IMPL,
                                            item.span,
                                            format!(
                                                "cannot implement `{}` for `{}`: neither the interface nor the type is defined in `{package}`",
                                                interface.name, self_ty
                                            ),
                                        )
                                        .with_note(
                                            "move the impl to the package that defines the type or the interface",
                                        ),
                                    );
                                }
                                for assoc_name in &interface.assoc_types {
                                    if !assoc_map.contains_key(assoc_name) {
                                        diagnostics.push(Diagnostic::error(
                                            codes::MISSING_ASSOC_TYPE,
                                            item.span,
                                            format!(
                                                "missing associated type `{assoc_name}` in `{}`",
                                                interface.name
                                            ),
                                        ));
                                    }
                                }
                                let mut self_subst = BTreeMap::new();
                                for (index, generic) in interface.generics.iter().enumerate() {
                                    if let Some(arg) = iface_args.get(index) {
                                        self_subst.insert(generic.name.clone(), arg.clone());
                                    }
                                }
                                for (method_name, iface_method) in &interface.methods {
                                    if iface_method.body.is_some() {
                                        continue;
                                    }
                                    let Some(impl_method) = method_map.get(method_name) else {
                                        diagnostics.push(Diagnostic::error(
                                            codes::MISSING_IMPL_METHOD,
                                            item.span,
                                            format!(
                                                "`{}` does not implement `{}::{method_name}`",
                                                self_ty, interface.name
                                            ),
                                        ));
                                        continue;
                                    };
                                    let expected_sig = substitute_signature(
                                        &iface_method.sig,
                                        &self_subst,
                                        &self_ty,
                                        &assoc_map,
                                    );
                                    if !signatures_compatible(&impl_method.sig, &expected_sig) {
                                        diagnostics.push(
                                            Diagnostic::error(
                                                codes::INVALID_IMPL,
                                                impl_method.span,
                                                format!(
                                                    "method `{method_name}` has signature `{}` but `{}::{method_name}` requires `{}`",
                                                    impl_method.sig.print(),
                                                    interface.name,
                                                    expected_sig.print()
                                                ),
                                            ),
                                        );
                                    }
                                }
                                // Provided methods become available on every
                                // implementing type, compiled once per iface.
                                for (method_name, iface_method) in &interface.methods {
                                    if iface_method.body.is_none() {
                                        continue;
                                    }
                                    method_map.entry(method_name.clone()).or_insert_with(|| {
                                        let sig = substitute_signature(
                                            &iface_method.sig,
                                            &self_subst,
                                            &self_ty,
                                            &assoc_map,
                                        );
                                        MethodInfo {
                                            sig,
                                            body: None,
                                            span: iface_method.span,
                                            docs: iface_method.docs.clone(),
                                            from_interface: Some(interface.name.clone()),
                                        }
                                    });
                                }
                            }
                        }
                    }

                    for method in methods {
                        if let Some(info) = method_map.get(&method.name) {
                            if method.body.is_some() {
                                body_work.push(BodyWork::Impl {
                                    impl_index: env.impls.len(),
                                    method: method.clone(),
                                    sig: info.sig.clone(),
                                });
                            }
                        }
                    }

                    if let Some(existing_iface) = &iface_name {
                        if env.impls.iter().any(|existing| {
                            existing.iface.as_deref() == Some(existing_iface.as_str())
                                && existing.self_ty == self_ty
                        }) {
                            diagnostics.push(
                                Diagnostic::error(
                                    codes::INVALID_IMPL,
                                    item.span,
                                    format!(
                                        "conflicting impl of `{existing_iface}` for `{self_ty}`"
                                    ),
                                )
                                .with_note(
                                    "Teal coherence is per-package and impls may not overlap",
                                ),
                            );
                        }
                    }

                    let impl_index = env.impls.len();
                    env.impls.push(ImplInfo {
                        package: package.to_string(),
                        generics: generic_infos,
                        self_ty: self_ty.clone(),
                        iface: iface_name,
                        iface_args,
                        assoc: assoc_map,
                        methods: method_map,
                        span: item.span,
                        public: item.vis == Visibility::Public,
                    });
                    impl_spans.insert(item.span, impl_index);
                }
                ItemKind::Fn(function) => {
                    let mut generic_infos = Vec::new();
                    let mut generic_names = BTreeSet::new();
                    for param in &function.generics.params {
                        generic_names.insert(param.name.clone());
                        let mut bounds = Vec::new();
                        for bound in &param.bounds {
                            match env.elaborate_bound(
                                bound,
                                &ElabCtx {
                                    generics: generic_names.clone(),
                                    ..Default::default()
                                },
                            ) {
                                Ok(b) => bounds.push(b),
                                Err(diag) => diagnostics.push(diag),
                            }
                        }
                        generic_infos.push(GenericInfo {
                            name: param.name.clone(),
                            bounds,
                        });
                    }
                    let ctx = ElabCtx {
                        generics: generic_names,
                        ..Default::default()
                    };
                    let sig = match env.elaborate_signature(
                        &function.name,
                        &generic_infos,
                        &function.params,
                        &function.ret,
                        &ctx,
                    ) {
                        Ok(sig) => sig,
                        Err(diag) => {
                            diagnostics.push(diag);
                            continue;
                        }
                    };
                    env.functions.insert(
                        function.name.clone(),
                        FunctionInfo {
                            sig: sig.clone(),
                            package: package.to_string(),
                            public: item.vis == Visibility::Public,
                            docs: item.docs.clone(),
                            span: item.span,
                        },
                    );
                    if let Some(_body) = &function.body {
                        body_work.push(BodyWork::Function {
                            function: function.clone(),
                            sig,
                        });
                    }
                }
                ItemKind::Const { name, ty, .. } => match env.elaborate(ty, &ElabCtx::default()) {
                    Ok(ty) => {
                        env.consts.insert(name.clone(), ty);
                        env.const_packages.insert(name.clone(), package.to_string());
                    }
                    Err(diag) => diagnostics.push(diag),
                },
                ItemKind::Macro(def) => {
                    env.macros.insert(def.name.clone(), def.clone());
                }
                _ => {}
            }
        }
    }
    let _ = impl_spans;

    // Phase 3: check every body independently.
    let mut functions_checked = 0;
    for work in body_work {
        let mut checker = BodyChecker::new(&env, &diagnostics, package);
        match &work {
            BodyWork::Function { function, sig, .. } => {
                if let Some(body) = &function.body {
                    checker.check_function_body(function, sig, body);
                }
            }
            BodyWork::Impl {
                impl_index,
                method,
                sig,
                ..
            } => {
                if let Some(body) = &method.body {
                    checker.check_impl_method_body(&env.impls[*impl_index], method, sig, body);
                }
            }
            BodyWork::Interface {
                iface, method, sig, ..
            } => {
                if let Some(body) = &method.body {
                    checker.check_interface_provided_body(iface, method, sig, body);
                }
            }
        }
        diagnostics.extend(checker.finish());
        functions_checked += 1;
    }

    // Constants are checked as standalone expressions.
    for module in modules {
        for item in &module.items {
            if let ItemKind::Const { name, ty, value } = &item.kind {
                let mut checker = BodyChecker::new(&env, &diagnostics, package);
                checker.check_const(name, ty, value);
                diagnostics.extend(checker.finish());
            }
        }
    }

    diagnostics.sort_by(|a, b| {
        (a.span.file, a.span.start, a.span.end, a.code).cmp(&(
            b.span.file,
            b.span.start,
            b.span.end,
            b.code,
        ))
    });
    diagnostics.dedup_by(|a, b| a.span == b.span && a.code == b.code && a.message == b.message);

    CheckResult {
        types_checked: env.types.len(),
        env,
        diagnostics,
        functions_checked,
    }
}

fn type_package_of(ty: &Type, env: &Env) -> Option<String> {
    match ty {
        Type::Named { name, .. } => env.types.get(name).map(|info| info.package.clone()),
        Type::Ref { inner, .. } => type_package_of(inner, env),
        Type::Slice(_) | Type::Tuple(_) | Type::Fn { .. } | Type::Bool | Type::Char | Type::Str => {
            Some("std.prelude".into())
        }
        Type::Int(_) | Type::Float(_) => Some("std.prelude".into()),
        Type::Param(_) | Type::SelfType => None,
        Type::Dyn { name, .. } => env.interfaces.get(name).map(|info| info.package.clone()),
        Type::Error | Type::Unit | Type::Never => None,
    }
}

enum BodyWork {
    Function {
        function: Function,
        sig: FnSig,
    },
    Impl {
        impl_index: usize,
        method: Method,
        sig: FnSig,
    },
    Interface {
        iface: String,
        method: Method,
        sig: FnSig,
    },
}

#[derive(Clone, Default)]
pub(crate) struct ElabCtx {
    generics: BTreeSet<String>,
    self_ty: Option<Type>,
    assoc: BTreeMap<String, Type>,
}

impl Env {
    pub fn builtin() -> Self {
        let mut env = Env {
            package: "std.prelude".into(),
            ..Default::default()
        };

        let span = Span::dummy();
        let t = |name: &str| Type::Param(name.into());
        let self_ref = Type::Ref {
            inner: Box::new(Type::SelfType),
            mutable: false,
        };
        let self_mut = Type::Ref {
            inner: Box::new(Type::SelfType),
            mutable: true,
        };

        // Enums
        env.types.insert(
            "Option".into(),
            TypeInfo {
                name: "Option".into(),
                package: "std.prelude".into(),
                public: true,
                generics: vec!["T".into()],
                kind: TypeKind::Enum {
                    variants: vec![
                        VariantInfo {
                            name: "Some".into(),
                            fields: Vec::new(),
                            tuple: vec![t("T")],
                            span,
                        },
                        VariantInfo {
                            name: "None".into(),
                            fields: Vec::new(),
                            tuple: Vec::new(),
                            span,
                        },
                    ],
                    open: false,
                },
                docs: vec!["A value that may be absent.".into()],
                span,
            },
        );
        env.types.insert(
            "Result".into(),
            TypeInfo {
                name: "Result".into(),
                package: "std.prelude".into(),
                public: true,
                generics: vec!["T".into(), "E".into()],
                kind: TypeKind::Enum {
                    variants: vec![
                        VariantInfo {
                            name: "Ok".into(),
                            fields: Vec::new(),
                            tuple: vec![t("T")],
                            span,
                        },
                        VariantInfo {
                            name: "Err".into(),
                            fields: Vec::new(),
                            tuple: vec![t("E")],
                            span,
                        },
                    ],
                    open: false,
                },
                docs: vec!["The only error channel in Teal.".into()],
                span,
            },
        );
        env.types.insert(
            "Vec".into(),
            TypeInfo {
                name: "Vec".into(),
                package: "std.prelude".into(),
                public: true,
                generics: vec!["T".into()],
                kind: TypeKind::Struct {
                    fields: vec![FieldInfo {
                        name: "len".into(),
                        ty: Type::Int(IntTy::Usize),
                    }],
                },
                docs: vec!["A growable array (intrinsics only in v0).".into()],
                span,
            },
        );
        env.types.insert(
            "Range".into(),
            TypeInfo {
                name: "Range".into(),
                package: "std.prelude".into(),
                public: true,
                generics: vec!["T".into()],
                kind: TypeKind::Struct {
                    fields: vec![
                        FieldInfo {
                            name: "start".into(),
                            ty: t("T"),
                        },
                        FieldInfo {
                            name: "end".into(),
                            ty: t("T"),
                        },
                    ],
                },
                docs: vec!["A half-open integer range.".into()],
                span,
            },
        );

        // Interfaces
        let mut eq_methods = BTreeMap::new();
        eq_methods.insert(
            "eq".into(),
            MethodInfo {
                sig: FnSig {
                    name: "eq".into(),
                    generics: Vec::new(),
                    params: vec![self_ref.clone(), self_ref.clone()],
                    param_names: vec!["self".into(), "other".into()],
                    ret: Type::Bool,
                },
                body: None,
                span,
                docs: Vec::new(),
                from_interface: None,
            },
        );
        env.interfaces.insert(
            "Eq".into(),
            InterfaceInfo {
                name: "Eq".into(),
                package: "std.prelude".into(),
                public: true,
                generics: Vec::new(),
                assoc_types: Vec::new(),
                methods: eq_methods,
                span,
                docs: vec!["Equality.".into()],
            },
        );

        let mut hash_methods = BTreeMap::new();
        hash_methods.insert(
            "hash".into(),
            MethodInfo {
                sig: FnSig {
                    name: "hash".into(),
                    generics: Vec::new(),
                    params: vec![self_ref.clone()],
                    param_names: vec!["self".into()],
                    ret: Type::Int(IntTy::U64),
                },
                body: None,
                span,
                docs: Vec::new(),
                from_interface: None,
            },
        );
        env.interfaces.insert(
            "Hash".into(),
            InterfaceInfo {
                name: "Hash".into(),
                package: "std.prelude".into(),
                public: true,
                generics: Vec::new(),
                assoc_types: Vec::new(),
                methods: hash_methods,
                span,
                docs: Vec::new(),
            },
        );

        let mut show_methods = BTreeMap::new();
        show_methods.insert(
            "show".into(),
            MethodInfo {
                sig: FnSig {
                    name: "show".into(),
                    generics: Vec::new(),
                    params: vec![self_ref.clone()],
                    param_names: vec!["self".into()],
                    ret: Type::Str,
                },
                body: None,
                span,
                docs: Vec::new(),
                from_interface: None,
            },
        );
        env.interfaces.insert(
            "Show".into(),
            InterfaceInfo {
                name: "Show".into(),
                package: "std.prelude".into(),
                public: true,
                generics: Vec::new(),
                assoc_types: Vec::new(),
                methods: show_methods,
                span,
                docs: Vec::new(),
            },
        );

        let mut from_methods = BTreeMap::new();
        from_methods.insert(
            "from".into(),
            MethodInfo {
                sig: FnSig {
                    name: "from".into(),
                    generics: Vec::new(),
                    params: vec![t("T")],
                    param_names: vec!["value".into()],
                    ret: Type::SelfType,
                },
                body: None,
                span,
                docs: Vec::new(),
                from_interface: None,
            },
        );
        env.interfaces.insert(
            "From".into(),
            InterfaceInfo {
                name: "From".into(),
                package: "std.prelude".into(),
                public: true,
                generics: vec![GenericInfo {
                    name: "T".into(),
                    bounds: Vec::new(),
                }],
                assoc_types: Vec::new(),
                methods: from_methods,
                span,
                docs: Vec::new(),
            },
        );

        let mut into_methods = BTreeMap::new();
        into_methods.insert(
            "into".into(),
            MethodInfo {
                sig: FnSig {
                    name: "into".into(),
                    generics: Vec::new(),
                    params: vec![self_ref.clone()],
                    param_names: vec!["self".into()],
                    ret: t("T"),
                },
                body: None,
                span,
                docs: Vec::new(),
                from_interface: None,
            },
        );
        env.interfaces.insert(
            "Into".into(),
            InterfaceInfo {
                name: "Into".into(),
                package: "std.prelude".into(),
                public: true,
                generics: vec![GenericInfo {
                    name: "T".into(),
                    bounds: Vec::new(),
                }],
                assoc_types: Vec::new(),
                methods: into_methods,
                span,
                docs: Vec::new(),
            },
        );

        let mut iterator_methods = BTreeMap::new();
        iterator_methods.insert(
            "next".into(),
            MethodInfo {
                sig: FnSig {
                    name: "next".into(),
                    generics: Vec::new(),
                    params: vec![self_mut.clone()],
                    param_names: vec!["self".into()],
                    ret: Type::Named {
                        name: "Option".into(),
                        args: vec![Type::named("Self::Item")],
                    },
                },
                body: None,
                span,
                docs: Vec::new(),
                from_interface: None,
            },
        );
        env.interfaces.insert(
            "Iterator".into(),
            InterfaceInfo {
                name: "Iterator".into(),
                package: "std.prelude".into(),
                public: true,
                generics: Vec::new(),
                assoc_types: vec!["Item".into()],
                methods: iterator_methods,
                span,
                docs: Vec::new(),
            },
        );

        // Intrinsic free functions used by the bundled macros.
        for (name, params, ret) in [
            ("unit_value", Vec::new(), Type::Unit),
            ("format_impl", Vec::new(), Type::Str),
            ("panic_impl", Vec::new(), Type::Never),
            ("assert_impl", vec![Type::Bool], Type::Unit),
        ] {
            env.functions.insert(
                name.into(),
                FunctionInfo {
                    sig: FnSig {
                        name: name.into(),
                        generics: Vec::new(),
                        params: params.clone(),
                        param_names: (0..params.len()).map(|i| format!("arg{i}")).collect(),
                        ret,
                    },
                    package: "std.prelude".into(),
                    public: true,
                    docs: Vec::new(),
                    span,
                },
            );
        }

        // Vec intrinsics.
        let mut vec_methods = BTreeMap::new();
        let vec_ty = Type::Named {
            name: "Vec".into(),
            args: vec![t("T")],
        };
        let push = FnSig {
            name: "push".into(),
            generics: Vec::new(),
            params: vec![self_mut.clone(), t("T")],
            param_names: vec!["self".into(), "value".into()],
            ret: Type::Unit,
        };
        vec_methods.insert(
            "push".into(),
            MethodInfo {
                sig: push,
                body: None,
                span,
                docs: Vec::new(),
                from_interface: None,
            },
        );
        let new = FnSig {
            name: "new".into(),
            generics: Vec::new(),
            params: Vec::new(),
            param_names: Vec::new(),
            ret: vec_ty.clone(),
        };
        vec_methods.insert(
            "new".into(),
            MethodInfo {
                sig: new,
                body: None,
                span,
                docs: Vec::new(),
                from_interface: None,
            },
        );
        let len = FnSig {
            name: "len".into(),
            generics: Vec::new(),
            params: vec![self_ref.clone()],
            param_names: vec!["self".into()],
            ret: Type::Int(IntTy::Usize),
        };
        vec_methods.insert(
            "len".into(),
            MethodInfo {
                sig: len,
                body: None,
                span,
                docs: Vec::new(),
                from_interface: None,
            },
        );
        let get = FnSig {
            name: "get".into(),
            generics: Vec::new(),
            params: vec![self_ref.clone(), Type::Int(IntTy::Usize)],
            param_names: vec!["self".into(), "index".into()],
            ret: Type::Named {
                name: "Option".into(),
                args: vec![t("T")],
            },
        };
        vec_methods.insert(
            "get".into(),
            MethodInfo {
                sig: get,
                body: None,
                span,
                docs: Vec::new(),
                from_interface: None,
            },
        );
        vec_methods.insert(
            "is_empty".into(),
            MethodInfo {
                sig: FnSig {
                    name: "is_empty".into(),
                    generics: Vec::new(),
                    params: vec![self_ref.clone()],
                    param_names: vec!["self".into()],
                    ret: Type::Bool,
                },
                body: None,
                span,
                docs: Vec::new(),
                from_interface: None,
            },
        );
        env.impls.push(ImplInfo {
            package: "std.prelude".into(),
            generics: vec![GenericInfo {
                name: "T".into(),
                bounds: Vec::new(),
            }],
            self_ty: vec_ty,
            iface: None,
            iface_args: Vec::new(),
            assoc: BTreeMap::new(),
            methods: vec_methods,
            span,
            public: true,
        });

        // Slice intrinsics (`vec![...]` expands to a list literal).
        let mut slice_methods = BTreeMap::new();
        slice_methods.insert(
            "len".into(),
            MethodInfo {
                sig: FnSig {
                    name: "len".into(),
                    generics: Vec::new(),
                    params: vec![self_ref.clone()],
                    param_names: vec!["self".into()],
                    ret: Type::Int(IntTy::Usize),
                },
                body: None,
                span,
                docs: Vec::new(),
                from_interface: None,
            },
        );
        slice_methods.insert(
            "is_empty".into(),
            MethodInfo {
                sig: FnSig {
                    name: "is_empty".into(),
                    generics: Vec::new(),
                    params: vec![self_ref.clone()],
                    param_names: vec!["self".into()],
                    ret: Type::Bool,
                },
                body: None,
                span,
                docs: Vec::new(),
                from_interface: None,
            },
        );
        slice_methods.insert(
            "get".into(),
            MethodInfo {
                sig: FnSig {
                    name: "get".into(),
                    generics: Vec::new(),
                    params: vec![self_ref.clone(), Type::Int(IntTy::Usize)],
                    param_names: vec!["self".into(), "index".into()],
                    ret: Type::Named {
                        name: "Option".into(),
                        args: vec![t("T")],
                    },
                },
                body: None,
                span,
                docs: Vec::new(),
                from_interface: None,
            },
        );
        env.impls.push(ImplInfo {
            package: "std.prelude".into(),
            generics: vec![GenericInfo {
                name: "T".into(),
                bounds: Vec::new(),
            }],
            self_ty: Type::Slice(Box::new(t("T"))),
            iface: None,
            iface_args: Vec::new(),
            assoc: BTreeMap::new(),
            methods: slice_methods,
            span,
            public: true,
        });

        // Option intrinsics.
        let option_ty = Type::Named {
            name: "Option".into(),
            args: vec![t("T")],
        };
        let mut option_methods = BTreeMap::new();
        for name in ["is_some", "is_none"] {
            option_methods.insert(
                name.to_string(),
                MethodInfo {
                    sig: FnSig {
                        name: name.into(),
                        generics: Vec::new(),
                        params: vec![self_ref.clone()],
                        param_names: vec!["self".into()],
                        ret: Type::Bool,
                    },
                    body: None,
                    span,
                    docs: Vec::new(),
                    from_interface: None,
                },
            );
        }
        env.impls.push(ImplInfo {
            package: "std.prelude".into(),
            generics: vec![GenericInfo {
                name: "T".into(),
                bounds: Vec::new(),
            }],
            self_ty: option_ty,
            iface: None,
            iface_args: Vec::new(),
            assoc: BTreeMap::new(),
            methods: option_methods,
            span,
            public: true,
        });

        env
    }

    pub fn import_tli(&mut self, tli: &Tli) {
        if tli.builtin || tli.package == self.package {
            return;
        }
        self.dependencies.insert(tli.package.clone());

        for ty in &tli.types {
            let generics = ty.generics.clone();
            let kind = match ty.kind.as_str() {
                "struct" => TypeKind::Struct {
                    fields: ty
                        .fields
                        .iter()
                        .filter_map(|(name, ty)| {
                            Some(FieldInfo {
                                name: name.clone(),
                                ty: parse_type(ty)?,
                            })
                        })
                        .collect(),
                },
                "enum" => TypeKind::Enum {
                    variants: ty
                        .variants
                        .iter()
                        .map(|v| VariantInfo {
                            name: v.name.clone(),
                            fields: v
                                .fields
                                .iter()
                                .filter_map(|(n, t)| {
                                    Some(FieldInfo {
                                        name: n.clone(),
                                        ty: parse_type(t)?,
                                    })
                                })
                                .collect(),
                            tuple: v.tuple.iter().filter_map(|t| parse_type(t)).collect(),
                            span: Span::dummy(),
                        })
                        .collect(),
                    open: ty.open,
                },
                "alias" => TypeKind::Alias {
                    target: ty
                        .alias
                        .as_deref()
                        .and_then(parse_type)
                        .unwrap_or(Type::Error),
                },
                _ => TypeKind::Opaque {
                    repr: ty
                        .repr
                        .as_deref()
                        .and_then(parse_type)
                        .unwrap_or(Type::Error),
                },
            };
            self.types.insert(
                ty.name.clone(),
                TypeInfo {
                    name: ty.name.clone(),
                    package: tli.package.clone(),
                    public: true,
                    generics,
                    kind,
                    docs: Vec::new(),
                    span: Span::dummy(),
                },
            );
        }

        for iface in &tli.interfaces {
            let mut methods = BTreeMap::new();
            for method in &iface.methods {
                methods.insert(
                    method.name.clone(),
                    MethodInfo {
                        sig: method.to_sig(),
                        // Provided methods are marked with an empty body so
                        // downstream impls are not required to repeat them.
                        body: if method.has_body {
                            Some(Block::empty(Span::dummy()))
                        } else {
                            None
                        },
                        span: Span::dummy(),
                        docs: Vec::new(),
                        from_interface: None,
                    },
                );
            }
            self.interfaces.insert(
                iface.name.clone(),
                InterfaceInfo {
                    name: iface.name.clone(),
                    package: tli.package.clone(),
                    public: true,
                    generics: iface
                        .generics
                        .iter()
                        .map(|g| GenericInfo {
                            name: g.clone(),
                            bounds: Vec::new(),
                        })
                        .collect(),
                    assoc_types: iface.assoc.clone(),
                    methods,
                    span: Span::dummy(),
                    docs: Vec::new(),
                },
            );
        }

        for imp in &tli.impls {
            let self_ty = parse_type(&imp.self_ty).unwrap_or(Type::Error);
            let mut methods = BTreeMap::new();
            for method in &imp.methods {
                methods.insert(
                    method.name.clone(),
                    MethodInfo {
                        sig: method.to_sig(),
                        body: None,
                        span: Span::dummy(),
                        docs: Vec::new(),
                        from_interface: None,
                    },
                );
            }
            self.impls.push(ImplInfo {
                package: tli.package.clone(),
                generics: imp
                    .generics
                    .iter()
                    .map(|g| GenericInfo {
                        name: g.clone(),
                        bounds: Vec::new(),
                    })
                    .collect(),
                self_ty,
                iface: imp.iface.clone(),
                iface_args: imp
                    .iface_args
                    .iter()
                    .filter_map(|t| parse_type(t))
                    .collect(),
                assoc: imp
                    .assoc
                    .iter()
                    .filter_map(|(k, v)| Some((k.clone(), parse_type(v)?)))
                    .collect(),
                methods,
                span: Span::dummy(),
                public: true,
            });
        }

        for function in &tli.functions {
            self.functions.insert(
                function.name.clone(),
                FunctionInfo {
                    sig: function.to_sig(),
                    package: tli.package.clone(),
                    public: true,
                    docs: Vec::new(),
                    span: Span::dummy(),
                },
            );
        }

        for (name, ty) in &tli.consts {
            if let Some(ty) = parse_type(ty) {
                self.const_packages
                    .insert(name.clone(), tli.package.clone());
                self.consts.insert(name.clone(), ty);
            }
        }
    }

    fn elaborate_method(
        &self,
        method: &Method,
        parent: &ElabCtx,
        _vis: Visibility,
    ) -> Result<MethodInfo, Diagnostic> {
        let mut generic_infos = Vec::new();
        let mut generics = parent.generics.clone();
        for param in &method.generics.params {
            generics.insert(param.name.clone());
            let mut bounds = Vec::new();
            for bound in &param.bounds {
                bounds.push(self.elaborate_bound(
                    bound,
                    &ElabCtx {
                        generics: generics.clone(),
                        self_ty: parent.self_ty.clone(),
                        assoc: parent.assoc.clone(),
                    },
                )?);
            }
            generic_infos.push(GenericInfo {
                name: param.name.clone(),
                bounds,
            });
        }
        let ctx = ElabCtx {
            generics,
            self_ty: parent.self_ty.clone(),
            assoc: parent.assoc.clone(),
        };
        let sig = self.elaborate_signature(
            &method.name,
            &generic_infos,
            &method.params,
            &method.ret,
            &ctx,
        )?;
        Ok(MethodInfo {
            sig,
            body: method.body.clone(),
            span: method.span,
            docs: method.docs.clone(),
            from_interface: None,
        })
    }

    fn elaborate_signature(
        &self,
        name: &str,
        generics: &[GenericInfo],
        params: &[Param],
        ret: &Option<TypeExpr>,
        ctx: &ElabCtx,
    ) -> Result<FnSig, Diagnostic> {
        let mut param_types = Vec::new();
        let mut param_names = Vec::new();
        for param in params {
            let ty = match &param.ty {
                Some(ty) => self.elaborate(ty, ctx)?,
                None => Type::Unit,
            };
            param_types.push(ty);
            param_names.push(param.name.clone());
        }
        let ret_ty = match ret {
            Some(ty) => self.elaborate(ty, ctx)?,
            None => Type::Unit,
        };
        Ok(FnSig {
            name: name.to_string(),
            generics: generics.to_vec(),
            params: param_types,
            param_names,
            ret: ret_ty,
        })
    }

    fn elaborate_bound(&self, bound: &Bound, ctx: &ElabCtx) -> Result<BoundInfo, Diagnostic> {
        let name = bound.path.last().cloned().unwrap_or_default();
        if let Some((params, ret)) = &bound.fn_sig {
            let mut param_types = Vec::new();
            for param in params {
                param_types.push(self.elaborate(param, ctx)?);
            }
            let ret = self.elaborate(ret, ctx)?;
            return Ok(BoundInfo {
                iface: "Fn".into(),
                args: Vec::new(),
                assoc: BTreeMap::new(),
                fn_sig: Some((param_types, ret)),
            });
        }
        if !matches!(
            name.as_str(),
            "Eq" | "Hash" | "Show" | "Into" | "From" | "Iterator" | "Clone" | "Debug"
        ) && !self.interfaces.contains_key(&name)
        {
            return Err(Diagnostic::error(
                codes::UNKNOWN_TYPE,
                bound.span,
                format!("unknown interface `{name}` in bound"),
            ));
        }
        let mut args = Vec::new();
        for arg in &bound.args {
            args.push(self.elaborate(arg, ctx)?);
        }
        let mut assoc = BTreeMap::new();
        for (key, value) in &bound.assoc {
            assoc.insert(key.clone(), self.elaborate(value, ctx)?);
        }
        Ok(BoundInfo {
            iface: name,
            args,
            assoc,
            fn_sig: None,
        })
    }

    pub(crate) fn elaborate(&self, ty: &TypeExpr, ctx: &ElabCtx) -> Result<Type, Diagnostic> {
        match &ty.kind {
            TypeExprKind::Unit => Ok(Type::Unit),
            TypeExprKind::Tuple(items) => {
                let mut out = Vec::new();
                for item in items {
                    out.push(self.elaborate(item, ctx)?);
                }
                Ok(Type::Tuple(out))
            }
            TypeExprKind::Ref { inner, mutable } => Ok(Type::Ref {
                inner: Box::new(self.elaborate(inner, ctx)?),
                mutable: *mutable,
            }),
            TypeExprKind::Slice(inner) => Ok(Type::Slice(Box::new(self.elaborate(inner, ctx)?))),
            TypeExprKind::Fn { params, ret } => {
                let mut out = Vec::new();
                for param in params {
                    out.push(self.elaborate(param, ctx)?);
                }
                Ok(Type::Fn {
                    params: out,
                    ret: Box::new(self.elaborate(ret, ctx)?),
                })
            }
            TypeExprKind::Dyn { path, args, assoc } => {
                let name = path.last().cloned().unwrap_or_default();
                let mut elaborated = Vec::new();
                for arg in args {
                    elaborated.push(self.elaborate(arg, ctx)?);
                }
                let mut assoc_out = BTreeMap::new();
                for (key, value) in assoc {
                    assoc_out.insert(key.clone(), self.elaborate(value, ctx)?);
                }
                Ok(Type::Dyn {
                    name,
                    args: elaborated,
                    assoc: assoc_out.into_iter().collect(),
                })
            }
            TypeExprKind::Path { path, args, assoc } => {
                let first = path.first().map(String::as_str).unwrap_or("");
                let last = path.last().map(String::as_str).unwrap_or("");

                if path.len() == 1 && first == "Self" {
                    return Ok(ctx.self_ty.clone().unwrap_or(Type::SelfType));
                }
                if path.len() == 2 && first == "Self" {
                    if let Some(bound) = ctx.assoc.get(last) {
                        return Ok(bound.clone());
                    }
                    return Ok(Type::named(format!("Self::{last}")));
                }
                if path.len() == 1 {
                    if ctx.generics.contains(first) {
                        return Ok(Type::Param(first.to_string()));
                    }
                }
                if path.len() == 2 && ctx.generics.contains(first) {
                    // `T::Assoc` resolves through the declared bounds.
                    if let Some(ty) = self.assoc_through_generic(first, last, ctx) {
                        return Ok(ty);
                    }
                }

                // Associated types written with a type path, e.g.
                // `Iterator<Item = i64>` handled via args below.
                if let Some(primitive) = Type::primitive(last) {
                    return Ok(primitive);
                }
                let name = last.to_string();
                let mut elaborated_args = Vec::new();
                for arg in args {
                    elaborated_args.push(self.elaborate(arg, ctx)?);
                }
                let _ = assoc;
                let Some(info) = self.types.get(&name) else {
                    return Err(Diagnostic::error(
                        codes::UNKNOWN_TYPE,
                        ty.span,
                        format!("unknown type `{name}`"),
                    ));
                };
                if let TypeKind::Alias { target } = &info.kind {
                    let mut subst = BTreeMap::new();
                    for (generic, arg) in info.generics.iter().zip(&elaborated_args) {
                        subst.insert(generic.clone(), arg.clone());
                    }
                    return Ok(target.substitute(&subst));
                }
                if info.generics.len() != elaborated_args.len() {
                    return Err(Diagnostic::error(
                        codes::WRONG_ARITY,
                        ty.span,
                        format!(
                            "type `{name}` takes {} type argument(s), found {}",
                            info.generics.len(),
                            elaborated_args.len()
                        ),
                    ));
                }
                Ok(Type::Named {
                    name,
                    args: elaborated_args,
                })
            }
        }
    }

    fn assoc_through_generic(&self, generic: &str, assoc: &str, ctx: &ElabCtx) -> Option<Type> {
        // Bounds are looked up from the current context only. This is exactly
        // the "definition-site bound checking" restriction: no impl graph
        // search is performed to discover an associated type.
        let _ = ctx;
        let _ = generic;
        let _ = assoc;
        None
    }

    pub fn enum_info(&self, ty: &Type) -> Option<(&TypeInfo, Vec<Type>)> {
        match ty {
            Type::Named { name, args } => {
                let info = self.types.get(name)?;
                if matches!(info.kind, TypeKind::Enum { .. }) {
                    Some((info, args.clone()))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn struct_fields(&self, ty: &Type) -> Option<Vec<(String, Type)>> {
        match ty {
            Type::Named { name, args } => {
                let info = self.types.get(name)?;
                match &info.kind {
                    TypeKind::Struct { fields } => {
                        let subst: BTreeMap<String, Type> = info
                            .generics
                            .iter()
                            .cloned()
                            .zip(args.iter().cloned())
                            .collect();
                        Some(
                            fields
                                .iter()
                                .map(|f| (f.name.clone(), f.ty.substitute(&subst)))
                                .collect(),
                        )
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    pub fn find_variant(&self, hint: Option<&str>, variant: &str) -> Option<(Type, VariantInfo)> {
        let matches = |info: &TypeInfo| -> Option<VariantInfo> {
            if let TypeKind::Enum { variants, .. } = &info.kind {
                variants.iter().find(|v| v.name == variant).cloned()
            } else {
                None
            }
        };
        if let Some(hint) = hint {
            if let Some(info) = self.types.get(hint) {
                if let Some(v) = matches(info) {
                    return Some((
                        Type::Named {
                            name: info.name.clone(),
                            args: info.param_types(),
                        },
                        v,
                    ));
                }
            }
        }
        for info in self.types.values() {
            if let Some(v) = matches(info) {
                return Some((
                    Type::Named {
                        name: info.name.clone(),
                        args: info.param_types(),
                    },
                    v,
                ));
            }
        }
        None
    }

    pub fn find_method(&self, ty: &Type, name: &str, ctx: &GenericsCtx) -> Option<FnSig> {
        let ty = ty.deref();
        match ty {
            Type::Param(param) => {
                for bound in ctx.bounds.get(param).into_iter().flatten() {
                    if bound.iface == "Fn" {
                        continue;
                    }
                    if let Some(iface) = self.interfaces.get(&bound.iface) {
                        if let Some(method) = iface.methods.get(name) {
                            let _ = &bound.args;
                            return Some(substitute_signature(
                                &method.sig,
                                &BTreeMap::new(),
                                ty,
                                &bound.assoc,
                            ));
                        }
                    }
                }
                None
            }
            Type::SelfType => {
                let iface_name = ctx.iface_self.as_ref()?;
                let iface = self.interfaces.get(iface_name)?;
                let method = iface.methods.get(name)?;
                Some(substitute_signature(
                    &method.sig,
                    &BTreeMap::new(),
                    ty,
                    &ctx.self_assoc,
                ))
            }
            Type::Dyn {
                name: iface_name,
                assoc,
                ..
            } => {
                let iface = self.interfaces.get(iface_name)?;
                let method = iface.methods.get(name)?;
                let assoc_map: BTreeMap<String, Type> = assoc.iter().cloned().collect();
                Some(substitute_signature(
                    &method.sig,
                    &BTreeMap::new(),
                    ty,
                    &assoc_map,
                ))
            }
            _ => {
                for imp in &self.impls {
                    if !imp.self_ty.head().eq_ignore_ascii_case(&ty.head())
                        && imp.self_ty.head() != ty.head()
                    {
                        continue;
                    }
                    let Some(method) = imp.methods.get(name) else {
                        continue;
                    };
                    let mut subst = BTreeMap::new();
                    if ty.matches_pattern(&imp.self_ty, &mut subst) {
                        let mut assoc = imp.assoc.clone();
                        for value in assoc.values_mut() {
                            *value = value.substitute(&subst);
                        }
                        return Some(substitute_signature(&method.sig, &subst, ty, &assoc));
                    }
                }
                None
            }
        }
    }

    pub fn impl_exists(&self, iface: &str, self_ty: &Type, args: &[Type]) -> bool {
        self.impls.iter().any(|imp| {
            if imp.iface.as_deref() != Some(iface) {
                return false;
            }
            let mut subst = BTreeMap::new();
            if !self_ty.matches_pattern(&imp.self_ty, &mut subst) {
                return false;
            }
            imp.iface_args
                .iter()
                .zip(args)
                .all(|(a, b)| a.substitute(&subst) == *b)
        })
    }

    pub fn from_impl_exists(&self, from: &Type, to: &Type) -> bool {
        self.impls.iter().any(|imp| {
            if imp.iface.as_deref() != Some("From") {
                return false;
            }
            let mut subst = BTreeMap::new();
            if !to.matches_pattern(&imp.self_ty, &mut subst) {
                return false;
            }
            imp.iface_args
                .first()
                .map(|arg| arg.substitute(&subst) == *from)
                .unwrap_or(false)
        })
    }

    pub fn iterator_item(&self, ty: &Type, ctx: &GenericsCtx) -> Option<Type> {
        match ty.deref() {
            Type::Slice(inner) => Some((**inner).clone()),
            Type::Named { name, args } if name == "Vec" => args.first().cloned(),
            Type::Named { name, args } if name == "Range" => args.first().cloned(),
            Type::Named { name, args } => {
                let dummy = Type::Named {
                    name: name.clone(),
                    args: args.clone(),
                };
                for imp in &self.impls {
                    if imp.iface.as_deref() == Some("Iterator") {
                        let mut subst = BTreeMap::new();
                        if dummy.matches_pattern(&imp.self_ty, &mut subst) {
                            if let Some(item) = imp.assoc.get("Item") {
                                return Some(item.substitute(&subst));
                            }
                        }
                    }
                }
                None
            }
            Type::Param(param) => {
                for bound in ctx.bounds.get(param).into_iter().flatten() {
                    if bound.iface == "Iterator" {
                        return Some(
                            bound
                                .assoc
                                .get("Item")
                                .cloned()
                                .unwrap_or_else(|| Type::named("Self::Item")),
                        );
                    }
                }
                None
            }
            _ => None,
        }
    }
}

fn substitute_signature(
    sig: &FnSig,
    subst: &BTreeMap<String, Type>,
    self_ty: &Type,
    assoc: &BTreeMap<String, Type>,
) -> FnSig {
    FnSig {
        name: sig.name.clone(),
        generics: sig
            .generics
            .iter()
            .map(|g| GenericInfo {
                name: g.name.clone(),
                bounds: g
                    .bounds
                    .iter()
                    .map(|b| BoundInfo {
                        iface: b.iface.clone(),
                        args: b
                            .args
                            .iter()
                            .map(|t| subst_type(t, subst, self_ty, assoc))
                            .collect(),
                        assoc: b
                            .assoc
                            .iter()
                            .map(|(k, v)| (k.clone(), subst_type(v, subst, self_ty, assoc)))
                            .collect(),
                        fn_sig: b.fn_sig.as_ref().map(|(p, r)| {
                            (
                                p.iter()
                                    .map(|t| subst_type(t, subst, self_ty, assoc))
                                    .collect(),
                                subst_type(r, subst, self_ty, assoc),
                            )
                        }),
                    })
                    .collect(),
            })
            .collect(),
        params: sig
            .params
            .iter()
            .map(|t| subst_type(t, subst, self_ty, assoc))
            .collect(),
        param_names: sig.param_names.clone(),
        ret: subst_type(&sig.ret, subst, self_ty, assoc),
    }
}

fn subst_type(
    ty: &Type,
    subst: &BTreeMap<String, Type>,
    self_ty: &Type,
    assoc: &BTreeMap<String, Type>,
) -> Type {
    match ty {
        Type::SelfType => self_ty.clone(),
        Type::Param(name) => subst.get(name).cloned().unwrap_or_else(|| ty.clone()),
        Type::Named { name, args } => {
            if let Some(rest) = name.strip_prefix("Self::") {
                if let Some(bound) = assoc.get(rest) {
                    return bound.clone();
                }
            }
            Type::Named {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|t| subst_type(t, subst, self_ty, assoc))
                    .collect(),
            }
        }
        Type::Ref { inner, mutable } => Type::Ref {
            inner: Box::new(subst_type(inner, subst, self_ty, assoc)),
            mutable: *mutable,
        },
        Type::Slice(inner) => Type::Slice(Box::new(subst_type(inner, subst, self_ty, assoc))),
        Type::Tuple(items) => Type::Tuple(
            items
                .iter()
                .map(|t| subst_type(t, subst, self_ty, assoc))
                .collect(),
        ),
        Type::Fn { params, ret } => Type::Fn {
            params: params
                .iter()
                .map(|t| subst_type(t, subst, self_ty, assoc))
                .collect(),
            ret: Box::new(subst_type(ret, subst, self_ty, assoc)),
        },
        Type::Dyn {
            name,
            args,
            assoc: da,
        } => Type::Dyn {
            name: name.clone(),
            args: args
                .iter()
                .map(|t| subst_type(t, subst, self_ty, assoc))
                .collect(),
            assoc: da
                .iter()
                .map(|(k, v)| (k.clone(), subst_type(v, subst, self_ty, assoc)))
                .collect(),
        },
        _ => ty.clone(),
    }
}

fn signatures_compatible(impl_sig: &FnSig, expected: &FnSig) -> bool {
    impl_sig.params.len() == expected.params.len()
        && impl_sig
            .params
            .iter()
            .zip(&expected.params)
            .all(|(a, b)| a.assignable_to(b) && b.assignable_to(a))
        && impl_sig.ret.assignable_to(&expected.ret)
        && expected.ret.assignable_to(&impl_sig.ret)
}

#[derive(Clone, Debug, Default)]
pub struct GenericsCtx {
    pub bounds: BTreeMap<String, Vec<BoundInfo>>,
    pub iface_self: Option<String>,
    pub self_assoc: BTreeMap<String, Type>,
}

// ------------------------------------------------------------- body checker

#[derive(Clone, Debug)]
struct VarInfo {
    ty: Type,
    mutable: bool,
}

pub(crate) struct BodyChecker<'a> {
    env: &'a Env,
    scopes: Vec<BTreeMap<String, VarInfo>>,
    errors: Vec<Diagnostic>,
    generics: GenericsCtx,
    self_ty: Option<Type>,
    ret: Type,
    iface_self: Option<String>,
    loop_depth: usize,
}

impl<'a> BodyChecker<'a> {
    fn new(env: &'a Env, _prior: &[Diagnostic], _package: &str) -> Self {
        Self {
            env,
            scopes: vec![BTreeMap::new()],
            errors: Vec::new(),
            generics: GenericsCtx::default(),
            self_ty: None,
            ret: Type::Unit,
            iface_self: None,
            loop_depth: 0,
        }
    }

    fn finish(self) -> Vec<Diagnostic> {
        self.errors
    }

    fn error(&mut self, code: &'static str, span: Span, message: impl Into<String>) {
        self.errors.push(Diagnostic::error(code, span, message));
    }

    fn error_note(
        &mut self,
        code: &'static str,
        span: Span,
        message: impl Into<String>,
        note: impl Into<String>,
    ) {
        self.errors
            .push(Diagnostic::error(code, span, message).with_note(note));
    }

    fn push_scope(&mut self) {
        self.scopes.push(BTreeMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn declare(&mut self, name: &str, ty: Type, mutable: bool) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), VarInfo { ty, mutable });
        }
    }

    fn lookup_var(&self, name: &str) -> Option<VarInfo> {
        for scope in self.scopes.iter().rev() {
            if let Some(var) = scope.get(name) {
                return Some(var.clone());
            }
        }
        None
    }

    fn generics_setup(&mut self, generics: &[GenericInfo]) {
        self.generics = GenericsCtx::default();
        for generic in generics {
            self.generics
                .bounds
                .insert(generic.name.clone(), generic.bounds.clone());
        }
    }

    fn elab_ctx(&self) -> ElabCtx {
        let mut assoc = self.generics.self_assoc.clone();
        if let Some(iface) = self.iface_self.as_ref() {
            if let Some(info) = self.env.interfaces.get(iface) {
                for name in &info.assoc_types {
                    assoc.insert(name.clone(), Type::named(format!("Self::{name}")));
                }
            }
        }
        ElabCtx {
            generics: self.generics.bounds.keys().cloned().collect(),
            self_ty: self.self_ty.clone(),
            assoc,
        }
    }

    fn elaborate(&mut self, ty: &TypeExpr) -> Type {
        let ctx = self.elab_ctx();
        match self.env.elaborate(ty, &ctx) {
            Ok(ty) => ty,
            Err(diag) => {
                self.errors.push(diag);
                Type::Error
            }
        }
    }

    fn check_function_body(&mut self, function: &Function, sig: &FnSig, body: &Block) {
        self.generics_setup(&sig.generics);
        self.self_ty = None;
        self.ret = sig.ret.clone();
        self.install_params(sig, None);
        let _ = function;
        let ret = self.ret.clone();
        let ty = self.check_block(body, Some(&ret));
        if !ty.assignable_to(&ret) {
            self.error(
                codes::TYPE_MISMATCH,
                body.span,
                format!(
                    "function body has type `{ty}` but `{}` returns `{}`",
                    sig.name, self.ret
                ),
            );
        }
    }

    fn check_impl_method_body(
        &mut self,
        imp: &ImplInfo,
        method: &Method,
        sig: &FnSig,
        body: &Block,
    ) {
        let mut generics = imp.generics.clone();
        generics.extend(sig.generics.iter().cloned());
        self.generics_setup(&generics);
        self.self_ty = Some(imp.self_ty.clone());
        self.iface_self = None;
        self.generics.self_assoc = imp.assoc.clone();
        self.ret = sig.ret.clone();
        self.install_params(sig, Some(&imp.self_ty));
        let _ = method;
        let ret = self.ret.clone();
        let ty = self.check_block(body, Some(&ret));
        if !ty.assignable_to(&ret) {
            self.error(
                codes::TYPE_MISMATCH,
                body.span,
                format!("method body has type `{ty}` but signature returns `{ret}`"),
            );
        }
    }

    fn check_interface_provided_body(
        &mut self,
        iface: &str,
        method: &Method,
        sig: &FnSig,
        body: &Block,
    ) {
        let mut generics = Vec::new();
        if let Some(info) = self.env.interfaces.get(iface) {
            generics.extend(info.generics.iter().cloned());
        }
        generics.extend(sig.generics.iter().cloned());
        self.generics_setup(&generics);
        self.self_ty = Some(Type::SelfType);
        self.iface_self = Some(iface.to_string());
        self.generics.iface_self = Some(iface.to_string());
        if let Some(info) = self.env.interfaces.get(iface) {
            for name in &info.assoc_types {
                self.generics
                    .self_assoc
                    .insert(name.clone(), Type::named(format!("Self::{name}")));
            }
        }
        self.ret = sig.ret.clone();
        self.install_params(sig, Some(&Type::SelfType));
        let _ = method;
        let ret = self.ret.clone();
        let ty = self.check_block(body, Some(&ret));
        if !ty.assignable_to(&ret) {
            self.error(
                codes::TYPE_MISMATCH,
                body.span,
                format!("provided method body has type `{ty}` but signature returns `{ret}`"),
            );
        }
    }

    fn check_const(&mut self, name: &str, ty: &TypeExpr, value: &Expr) {
        let expected = self.elaborate(ty);
        let actual = self.expr_type(value, Some(&expected));
        if !actual.assignable_to(&expected) {
            self.error(
                codes::TYPE_MISMATCH,
                value.span,
                format!("constant `{name}` has type `{actual}` but is declared `{expected}`"),
            );
        }
    }

    fn install_params(&mut self, sig: &FnSig, self_ty: Option<&Type>) {
        for (index, (ty, name)) in sig.params.iter().zip(&sig.param_names).enumerate() {
            let is_self = name == "self";
            let mut ty = ty.clone();
            if let Some(self_ty) = self_ty {
                ty = subst_type(&ty, &BTreeMap::new(), self_ty, &BTreeMap::new());
            }
            let mutable = if is_self {
                match &ty {
                    Type::Ref { mutable, .. } => *mutable,
                    _ => true,
                }
            } else {
                true
            };
            // In a method body `self` has the receiver's nominal type, not
            // `&mut Self`; field access is direct and mutation is permitted
            // only through `&mut self`.
            if is_self {
                let receiver_ty = match &ty {
                    Type::Ref { inner, .. } => (**inner).clone(),
                    other => other.clone(),
                };
                self.declare(name, receiver_ty, mutable);
            } else {
                self.declare(name, ty, mutable);
            }
            let _ = index;
        }
    }

    // ------------------------------------------------------------- statements

    fn check_block(&mut self, block: &Block, expected: Option<&Type>) -> Type {
        self.push_scope();
        let mut last_type = Type::Unit;
        let count = block.stmts.len();
        for (index, stmt) in block.stmts.iter().enumerate() {
            let is_last = index + 1 == count;
            last_type = self.check_stmt(stmt, if is_last { expected } else { None });
        }
        self.pop_scope();
        // A `;`-terminated final expression makes the block `()`. Diverging
        // statements such as `return` keep their `!` type.
        match block.stmts.last() {
            Some(stmt)
                if stmt.semi && matches!(stmt.kind, StmtKind::Expr(_)) && !last_type.is_never() =>
            {
                Type::Unit
            }
            _ => last_type,
        }
    }

    fn check_stmt(&mut self, stmt: &Stmt, expected: Option<&Type>) -> Type {
        match &stmt.kind {
            StmtKind::Let {
                pattern,
                mutable,
                ty,
                value,
                else_block,
            } => {
                let annotated = ty.as_ref().map(|ty| self.elaborate(ty));
                let value_ty = if let Some(annotated) = &annotated {
                    self.expr_type(value, Some(annotated))
                } else {
                    self.expr_type(value, None)
                };
                if let Some(annotated) = &annotated {
                    if !value_ty.assignable_to(annotated) {
                        self.error(
                            codes::TYPE_MISMATCH,
                            value.span,
                            format!(
                                "`let` binding expects `{annotated}` but the value has type `{value_ty}`"
                            ),
                        );
                    }
                }
                let binding_ty = annotated.unwrap_or(value_ty);
                let mut bindings = Vec::new();
                self.check_pattern(pattern, &binding_ty, &mut bindings);
                for (name, var_ty) in bindings {
                    self.declare(&name, var_ty, *mutable);
                }
                if let Some(else_block) = else_block {
                    let diverges = block_diverges(else_block);
                    let else_ty = self.check_block(else_block, Some(&Type::Never));
                    if !diverges && !else_ty.is_never() {
                        self.error(
                            codes::LET_ELSE_DIVERGE,
                            else_block.span,
                            "`let ... else` block must diverge (return, break, continue, or panic)",
                        );
                    }
                }
                Type::Unit
            }
            StmtKind::Assign { target, op, value } => {
                let target_ty = self.expr_type(target, None);
                self.check_assign_mutability(target);
                let mut value_ty = self.expr_type(value, Some(&target_ty));
                if let Some(op) = op {
                    value_ty = self.binary_result(*op, &target_ty, &value_ty, value.span);
                }
                if !value_ty.assignable_to(&target_ty) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        value.span,
                        format!("cannot assign `{value_ty}` to a target of type `{target_ty}`"),
                    );
                }
                Type::Unit
            }
            StmtKind::Defer(expr) => {
                self.expr_type(expr, None);
                Type::Unit
            }
            StmtKind::Expr(expr) => self.expr_type(expr, expected),
            StmtKind::Return(value) => {
                let expected = self.ret.clone();
                match value {
                    Some(expr) => {
                        let ty = self.expr_type(expr, Some(&expected));
                        if !ty.assignable_to(&expected) {
                            self.error(
                                codes::TYPE_MISMATCH,
                                expr.span,
                                format!(
                                    "returning `{ty}` from a function that returns `{expected}`"
                                ),
                            );
                        }
                    }
                    None => {
                        if !expected.is_unit() && !expected.is_never() {
                            self.error(
                                codes::TYPE_MISMATCH,
                                stmt.span,
                                format!(
                                    "this function returns `{expected}`, so `return` needs a value"
                                ),
                            );
                        }
                    }
                }
                Type::Never
            }
            StmtKind::Break => {
                if self.loop_depth == 0 {
                    self.error(codes::TYPE_MISMATCH, stmt.span, "`break` outside of a loop");
                }
                Type::Never
            }
            StmtKind::Continue => {
                if self.loop_depth == 0 {
                    self.error(
                        codes::TYPE_MISMATCH,
                        stmt.span,
                        "`continue` outside of a loop",
                    );
                }
                Type::Never
            }
            StmtKind::Item(_) => Type::Unit,
        }
    }

    fn check_assign_mutability(&mut self, target: &Expr) {
        let root = assignment_root(target);
        if let Some((name, span)) = root {
            if let Some(var) = self.lookup_var(&name) {
                if !var.mutable {
                    self.error(
                        codes::ASSIGN_IMMUTABLE,
                        span,
                        format!("cannot assign to immutable binding `{name}`"),
                    );
                }
            }
        }
    }

    // ------------------------------------------------------------- patterns

    fn check_pattern(&mut self, pattern: &Pattern, ty: &Type, bindings: &mut Vec<(String, Type)>) {
        match &pattern.kind {
            PatKind::Wildcard => {}
            PatKind::Binding { name, sub } => {
                bindings.push((name.clone(), ty.clone()));
                if let Some(sub) = sub {
                    self.check_pattern(sub, ty, bindings);
                }
            }
            PatKind::Ref { pattern, .. } => {
                let inner = ty.deref().clone();
                self.check_pattern(pattern, &inner, bindings);
            }
            PatKind::Tuple(items) => {
                let types = match ty {
                    Type::Tuple(types) => types.clone(),
                    Type::Unit => Vec::new(),
                    other => {
                        self.error(
                            codes::TYPE_MISMATCH,
                            pattern.span,
                            format!("tuple pattern cannot match `{other}`"),
                        );
                        vec![Type::Error; items.len()]
                    }
                };
                if types.len() != items.len() {
                    self.error(
                        codes::WRONG_ARITY,
                        pattern.span,
                        format!(
                            "tuple pattern has {} elements but `{ty}` has {}",
                            items.len(),
                            types.len()
                        ),
                    );
                }
                for (item, item_ty) in items.iter().zip(types.iter()) {
                    self.check_pattern(item, item_ty, bindings);
                }
            }
            PatKind::Or(alternatives) => {
                let mut first: Option<Vec<(String, Type)>> = None;
                for alternative in alternatives {
                    let mut branch = Vec::new();
                    self.check_pattern(alternative, ty, &mut branch);
                    match &first {
                        None => first = Some(branch),
                        Some(existing) => {
                            let names: BTreeSet<_> = existing.iter().map(|(n, _)| n).collect();
                            let other_names: BTreeSet<_> = branch.iter().map(|(n, _)| n).collect();
                            if names != other_names {
                                self.error(
                                    codes::TYPE_MISMATCH,
                                    alternative.span,
                                    "all alternatives of an or-pattern must bind the same names",
                                );
                            }
                        }
                    }
                }
                bindings.extend(first.unwrap_or_default());
            }
            PatKind::Literal(literal) => {
                let literal_ty = literal_type(literal);
                if !literal_matches(&literal_ty, ty) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        pattern.span,
                        format!(
                            "pattern of type `{literal_ty}` cannot match a value of type `{ty}`"
                        ),
                    );
                }
            }
            PatKind::Range { lo, hi, .. } => {
                let lo_ty = literal_type(lo);
                let hi_ty = literal_type(hi);
                if !lo_ty.is_numeric() && !matches!(lo_ty, Type::Char) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        pattern.span,
                        "range patterns require integer, float, or character bounds",
                    );
                }
                if !lo_ty.assignable_to(ty) || !hi_ty.assignable_to(ty) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        pattern.span,
                        format!("range pattern does not match `{ty}`"),
                    );
                }
            }
            PatKind::Path { path, args } => {
                let hint = if path.len() > 1 {
                    path.first().map(String::as_str)
                } else {
                    None
                };
                let variant_name = path.last().cloned().unwrap_or_default();
                match self.env.find_variant(hint, &variant_name) {
                    None => {
                        self.error(
                            codes::UNKNOWN_VARIANT,
                            pattern.span,
                            format!("unknown variant `{}`", path.join("::")),
                        );
                    }
                    Some((variant_ty, variant)) => {
                        if variant_ty.head() != ty.deref().head() && ty.deref().head() != "<error>"
                        {
                            self.error(
                                codes::TYPE_MISMATCH,
                                pattern.span,
                                format!(
                                    "variant `{}` belongs to `{variant_ty}`, but the scrutinee is `{ty}`",
                                    variant.name
                                ),
                            );
                        }
                        let (enum_type, arg_types) =
                            self.bind_variant_types(&variant_ty, &variant, ty);
                        if args.len() != variant.tuple.len()
                            && !(args.is_empty() && variant.tuple.is_empty())
                        {
                            self.error(
                                codes::WRONG_ARITY,
                                pattern.span,
                                format!(
                                    "variant `{}` takes {} field(s), pattern has {}",
                                    variant.name,
                                    variant.tuple.len(),
                                    args.len()
                                ),
                            );
                        }
                        for (arg, arg_ty) in args.iter().zip(arg_types.iter()) {
                            self.check_pattern(arg, arg_ty, bindings);
                        }
                        let _ = enum_type;
                    }
                }
            }
            PatKind::Struct { path, fields, rest } => {
                let hint = if path.len() > 1 {
                    path.first().map(String::as_str)
                } else {
                    None
                };
                let name = path.last().cloned().unwrap_or_default();
                // Struct variant or plain struct.
                if let Some((variant_ty, variant)) = self.env.find_variant(hint, &name) {
                    let (_, _) = self.bind_variant_types(&variant_ty, &variant, ty);
                    let mut seen = BTreeSet::new();
                    for (field_name, field_pattern) in fields {
                        let Some(field) = variant.fields.iter().find(|f| &f.name == field_name)
                        else {
                            self.error(
                                codes::UNKNOWN_FIELD,
                                field_pattern.span,
                                format!("variant `{}` has no field `{field_name}`", variant.name),
                            );
                            continue;
                        };
                        seen.insert(field_name.clone());
                        let field_ty = self.substitute_variant_field(&variant_ty, ty, &field.ty);
                        self.check_pattern(field_pattern, &field_ty, bindings);
                    }
                    if !rest {
                        for field in &variant.fields {
                            if !seen.contains(&field.name) {
                                self.error(
                                    codes::MISSING_FIELD,
                                    pattern.span,
                                    format!(
                                        "pattern for `{}` is missing field `{}`",
                                        variant.name, field.name
                                    ),
                                );
                            }
                        }
                    }
                    return;
                }
                if let Some(struct_ty) = self.env.types.get(&name) {
                    if let TypeKind::Struct { fields: def_fields } = &struct_ty.kind {
                        let self_ty = Type::Named {
                            name: struct_ty.name.clone(),
                            args: struct_ty.param_types(),
                        };
                        let subst: BTreeMap<String, Type> = struct_ty
                            .generics
                            .iter()
                            .cloned()
                            .zip(match ty {
                                Type::Named { args, .. }
                                    if args.len() == struct_ty.generics.len() =>
                                {
                                    args.clone()
                                }
                                _ => struct_ty.param_types(),
                            })
                            .collect();
                        let _ = self_ty;
                        let mut seen = BTreeSet::new();
                        for (field_name, field_pattern) in fields {
                            let Some(field) = def_fields.iter().find(|f| &f.name == field_name)
                            else {
                                self.error(
                                    codes::UNKNOWN_FIELD,
                                    field_pattern.span,
                                    format!("`{name}` has no field `{field_name}`"),
                                );
                                continue;
                            };
                            seen.insert(field_name.clone());
                            let field_ty = field.ty.substitute(&subst);
                            self.check_pattern(field_pattern, &field_ty, bindings);
                        }
                        if !rest {
                            for field in def_fields {
                                if !seen.contains(&field.name) {
                                    self.error(
                                        codes::MISSING_FIELD,
                                        pattern.span,
                                        format!(
                                            "pattern for `{name}` is missing field `{}`",
                                            field.name
                                        ),
                                    );
                                }
                            }
                        }
                    }
                } else {
                    self.error(
                        codes::UNKNOWN_TYPE,
                        pattern.span,
                        format!("unknown type or variant `{}`", path.join("::")),
                    );
                }
            }
        }
    }

    fn bind_variant_types(
        &self,
        variant_ty: &Type,
        variant: &VariantInfo,
        matched_ty: &Type,
    ) -> (Type, Vec<Type>) {
        let subst = match (variant_ty, matched_ty) {
            (Type::Named { args: va, .. }, Type::Named { args: ma, .. })
                if va.len() == ma.len() =>
            {
                va.iter()
                    .cloned()
                    .zip(ma.iter().cloned())
                    .collect::<BTreeMap<_, _>>()
            }
            _ => BTreeMap::new(),
        };
        let _ = subst;
        let arg_types = variant
            .tuple
            .iter()
            .map(|t| self.substitute_variant_field(variant_ty, matched_ty, t))
            .collect();
        (variant_ty.clone(), arg_types)
    }

    fn substitute_variant_field(
        &self,
        variant_ty: &Type,
        matched_ty: &Type,
        field_ty: &Type,
    ) -> Type {
        if let (Type::Named { name, args: va }, Type::Named { args: ma, .. }) =
            (variant_ty, matched_ty)
        {
            if va.len() == ma.len() {
                if let Some(info) = self.env.types.get(name) {
                    let mapping: BTreeMap<String, Type> = info
                        .generics
                        .iter()
                        .cloned()
                        .zip(ma.iter().cloned())
                        .collect();
                    return field_ty.substitute(&mapping);
                }
            }
        }
        field_ty.clone()
    }

    // ------------------------------------------------------------ expressions

    fn expr_type(&mut self, expr: &Expr, expected: Option<&Type>) -> Type {
        match &expr.kind {
            ExprKind::Int { suffix, .. } => {
                if let Some(suffix) = suffix {
                    if let Some(ty) = Type::primitive(suffix) {
                        return ty;
                    }
                }
                if let Some(expected) = expected {
                    if expected.is_integer() {
                        return expected.clone();
                    }
                    if expected.is_float() {
                        return expected.clone();
                    }
                }
                Type::Int(IntTy::I32)
            }
            ExprKind::Float { suffix, .. } => {
                if let Some(suffix) = suffix {
                    if let Some(ty) = Type::primitive(suffix) {
                        return ty;
                    }
                }
                if let Some(expected) = expected {
                    if expected.is_float() {
                        return expected.clone();
                    }
                }
                Type::Float(crate::types::FloatTy::F64)
            }
            ExprKind::Str(_) => Type::Str,
            ExprKind::Char(_) => Type::Char,
            ExprKind::Bool(_) => Type::Bool,
            ExprKind::SelfValue => self.self_ty.clone().unwrap_or_else(|| {
                self.error(
                    codes::UNKNOWN_NAME,
                    expr.span,
                    "`self` is only available inside an impl or interface method",
                );
                Type::Error
            }),
            ExprKind::Path { path, args } => self.path_type(expr, path, args, expected),
            ExprKind::Unary { op, expr: inner } => match op {
                UnOp::Neg => {
                    let ty = self.expr_type(inner, expected);
                    if !ty.is_numeric() && !ty.is_error() {
                        self.error(
                            codes::BAD_OPERATOR,
                            expr.span,
                            format!("cannot negate a value of type `{ty}`"),
                        );
                    }
                    ty
                }
                UnOp::Not => {
                    let ty = self.expr_type(inner, Some(&Type::Bool));
                    if !ty.is_bool() && !ty.is_error() {
                        self.error(
                            codes::BAD_OPERATOR,
                            expr.span,
                            format!("`!` expects `bool`, found `{ty}`"),
                        );
                    }
                    Type::Bool
                }
                UnOp::Ref | UnOp::RefMut => {
                    let ty = self.expr_type(inner, None);
                    Type::Ref {
                        inner: Box::new(ty),
                        mutable: matches!(op, UnOp::RefMut),
                    }
                }
            },
            ExprKind::Binary { op, lhs, rhs } => {
                let lhs_ty = self.expr_type(lhs, expected);
                let rhs_ty = self.expr_type(rhs, Some(&lhs_ty));
                self.binary_result(*op, &lhs_ty, &rhs_ty, expr.span)
            }
            ExprKind::Range { lo, hi, .. } => {
                let lo_ty = lo
                    .as_ref()
                    .map(|expr| self.expr_type(expr, Some(&Type::Int(IntTy::I64))))
                    .unwrap_or(Type::Int(IntTy::I64));
                let hi_ty = hi
                    .as_ref()
                    .map(|expr| self.expr_type(expr, Some(&lo_ty)))
                    .unwrap_or(lo_ty.clone());
                if !lo_ty.is_integer() && !lo_ty.is_error() {
                    self.error(
                        codes::TYPE_MISMATCH,
                        expr.span,
                        "only integer ranges are supported",
                    );
                }
                if !hi_ty.assignable_to(&lo_ty) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        expr.span,
                        format!("range bounds have different types: `{lo_ty}` and `{hi_ty}`"),
                    );
                }
                Type::Named {
                    name: "Range".into(),
                    args: vec![lo_ty],
                }
            }
            ExprKind::Call { callee, args } => self.check_call(expr, callee, args, expected),
            ExprKind::MethodCall {
                receiver,
                name,
                args,
                span,
            } => self.check_method_call(expr, receiver, name, args, *span, expected),
            ExprKind::Field { base, name } => {
                let base_ty = self.expr_type(base, None);
                self.field_type(&base_ty, name, expr.span)
            }
            ExprKind::Index { base, index } => {
                let base_ty = self.expr_type(base, None);
                let index_ty = self.expr_type(index, Some(&Type::Int(IntTy::Usize)));
                if !index_ty.is_integer() && !index_ty.is_error() {
                    self.error(
                        codes::TYPE_MISMATCH,
                        index.span,
                        format!("index must be an integer, found `{index_ty}`"),
                    );
                }
                match base_ty.deref() {
                    Type::Slice(inner) => (**inner).clone(),
                    Type::Named { name, args } if name == "Vec" => {
                        args.first().cloned().unwrap_or(Type::Error)
                    }
                    Type::Error => Type::Error,
                    other => {
                        self.error(
                            codes::TYPE_MISMATCH,
                            expr.span,
                            format!("cannot index a value of type `{other}`"),
                        );
                        Type::Error
                    }
                }
            }
            ExprKind::Try(inner) => self.try_type(expr, inner),
            ExprKind::Cast { expr: inner, ty } => {
                let source = self.expr_type(inner, None);
                let target = self.elaborate(ty);
                let valid = (source.is_numeric() && target.is_numeric())
                    || (source.is_integer() && matches!(target, Type::Char))
                    || (matches!(source, Type::Char) && target.is_integer())
                    || source.is_error()
                    || target.is_error()
                    || source == target;
                if !valid {
                    self.error(
                        codes::TYPE_MISMATCH,
                        expr.span,
                        format!("cannot cast `{source}` to `{target}`"),
                    );
                }
                target
            }
            ExprKind::Block(block) => self.check_block(block, expected),
            ExprKind::Match { scrutinee, arms } => {
                let scrutinee_ty = self.expr_type(scrutinee, None);
                let mut result = expected.cloned();
                for arm in arms {
                    self.push_scope();
                    let mut bindings = Vec::new();
                    self.check_pattern(&arm.pattern, &scrutinee_ty, &mut bindings);
                    for (name, ty) in bindings {
                        self.declare(&name, ty, false);
                    }
                    if let Some(guard) = &arm.guard {
                        let guard_ty = self.expr_type(guard, Some(&Type::Bool));
                        if !guard_ty.is_bool() && !guard_ty.is_error() {
                            self.error(
                                codes::TYPE_MISMATCH,
                                guard.span,
                                format!("match guard must be `bool`, found `{guard_ty}`"),
                            );
                        }
                    }
                    let arm_ty = self.expr_type(&arm.body, result.as_ref());
                    self.pop_scope();
                    match &result {
                        None => result = Some(arm_ty),
                        Some(existing) => {
                            if !arm_ty.assignable_to(existing)
                                && !existing.assignable_to(&arm_ty)
                                && !arm_ty.is_never()
                            {
                                self.error(
                                    codes::TYPE_MISMATCH,
                                    arm.body.span,
                                    format!(
                                        "match arms have incompatible types: `{existing}` and `{arm_ty}`"
                                    ),
                                );
                            }
                        }
                    }
                }
                self.check_exhaustiveness(&scrutinee_ty, arms, expr.span);
                result.unwrap_or(Type::Unit)
            }
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let cond_ty = self.expr_type(cond, Some(&Type::Bool));
                if !cond_ty.is_bool() && !cond_ty.is_error() {
                    self.error(
                        codes::TYPE_MISMATCH,
                        cond.span,
                        format!("`if` condition must be `bool`, found `{cond_ty}`"),
                    );
                }
                let then_ty = self.check_block(then_branch, expected);
                match else_branch {
                    None => {
                        if !then_ty.is_unit() && !then_ty.is_never() {
                            self.error(
                                codes::TYPE_MISMATCH,
                                then_branch.span,
                                format!(
                                    "`if` without `else` has type `()`, but this branch has type `{then_ty}`"
                                ),
                            );
                        }
                        Type::Unit
                    }
                    Some(else_branch) => {
                        let else_ty = self.expr_type(else_branch, expected.or(Some(&then_ty)));
                        if !then_ty.assignable_to(&else_ty) && !else_ty.assignable_to(&then_ty) {
                            self.error(
                                codes::TYPE_MISMATCH,
                                expr.span,
                                format!(
                                    "`if` branches have incompatible types: `{then_ty}` and `{else_ty}`"
                                ),
                            );
                        }
                        then_ty
                    }
                }
            }
            ExprKind::IfLet {
                pattern,
                scrutinee,
                then_branch,
                else_branch,
            } => {
                let scrutinee_ty = self.expr_type(scrutinee, None);
                self.push_scope();
                let mut bindings = Vec::new();
                self.check_pattern(pattern, &scrutinee_ty, &mut bindings);
                for (name, ty) in bindings {
                    self.declare(&name, ty, false);
                }
                let then_ty = self.check_block(then_branch, expected);
                self.pop_scope();
                match else_branch {
                    None => {
                        if !then_ty.is_unit() && !then_ty.is_never() {
                            self.error(
                                codes::TYPE_MISMATCH,
                                then_branch.span,
                                format!(
                                    "`if let` without `else` has type `()`, but this branch has type `{then_ty}`"
                                ),
                            );
                        }
                        Type::Unit
                    }
                    Some(else_branch) => {
                        let else_ty = self.expr_type(else_branch, expected.or(Some(&then_ty)));
                        if !then_ty.assignable_to(&else_ty) && !else_ty.assignable_to(&then_ty) {
                            self.error(
                                codes::TYPE_MISMATCH,
                                expr.span,
                                format!(
                                    "`if let` branches have incompatible types: `{then_ty}` and `{else_ty}`"
                                ),
                            );
                        }
                        then_ty
                    }
                }
            }
            ExprKind::While { cond, body } => {
                let cond_ty = self.expr_type(cond, Some(&Type::Bool));
                if !cond_ty.is_bool() && !cond_ty.is_error() {
                    self.error(
                        codes::TYPE_MISMATCH,
                        cond.span,
                        format!("`while` condition must be `bool`, found `{cond_ty}`"),
                    );
                }
                self.loop_depth += 1;
                self.check_block(body, Some(&Type::Unit));
                self.loop_depth -= 1;
                Type::Unit
            }
            ExprKind::WhileLet {
                pattern,
                scrutinee,
                body,
            } => {
                let scrutinee_ty = self.expr_type(scrutinee, None);
                self.push_scope();
                let mut bindings = Vec::new();
                self.check_pattern(pattern, &scrutinee_ty, &mut bindings);
                for (name, ty) in bindings {
                    self.declare(&name, ty, false);
                }
                self.loop_depth += 1;
                self.check_block(body, Some(&Type::Unit));
                self.loop_depth -= 1;
                self.pop_scope();
                Type::Unit
            }
            ExprKind::Loop { body } => {
                self.loop_depth += 1;
                self.check_block(body, Some(&Type::Unit));
                self.loop_depth -= 1;
                Type::Unit
            }
            ExprKind::For {
                pattern,
                iter,
                body,
            } => {
                let iter_ty = self.expr_type(iter, None);
                let item_ty = self
                    .env
                    .iterator_item(&iter_ty, &self.generics)
                    .unwrap_or_else(|| {
                        self.error(
                            codes::TYPE_MISMATCH,
                            iter.span,
                            format!(
                                "`{iter_ty}` is not iterable; implement `Iterator` for it or use a slice/range"
                            ),
                        );
                        Type::Error
                    });
                self.push_scope();
                let mut bindings = Vec::new();
                self.check_pattern(pattern, &item_ty, &mut bindings);
                for (name, ty) in bindings {
                    self.declare(&name, ty, false);
                }
                self.loop_depth += 1;
                self.check_block(body, Some(&Type::Unit));
                self.loop_depth -= 1;
                self.pop_scope();
                Type::Unit
            }
            ExprKind::Closure { params, ret, body } => {
                let expected_fn = match expected {
                    Some(Type::Fn { params, ret }) => Some((params.clone(), (**ret).clone())),
                    _ => None,
                };
                let mut param_types = Vec::new();
                self.push_scope();
                for (index, param) in params.iter().enumerate() {
                    let ty = match &param.ty {
                        Some(ty) => self.elaborate(ty),
                        None => match &expected_fn {
                            Some((params, _)) if params.len() == params.len() => {
                                params.get(index).cloned().unwrap_or_else(|| {
                                    self.error(
                                        codes::TYPE_MISMATCH,
                                        param.span,
                                        "cannot infer closure parameter type; annotate it",
                                    );
                                    Type::Error
                                })
                            }
                            _ => {
                                self.error(
                                    codes::TYPE_MISMATCH,
                                    param.span,
                                    "closure parameter needs an explicit type unless a typed expected type is available",
                                );
                                Type::Error
                            }
                        },
                    };
                    param_types.push(ty.clone());
                    self.declare(&param.name, ty, false);
                }
                let ret_ty = match ret {
                    Some(ret) => self.elaborate(ret),
                    None => expected_fn
                        .as_ref()
                        .map(|(_, ret)| ret.clone())
                        .unwrap_or(Type::Unit),
                };
                let body_ty = self.expr_type(body, Some(&ret_ty));
                self.pop_scope();
                if !body_ty.assignable_to(&ret_ty) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        body.span,
                        format!("closure body has type `{body_ty}` but `{ret_ty}` was expected"),
                    );
                }
                Type::Fn {
                    params: param_types,
                    ret: Box::new(ret_ty),
                }
            }
            ExprKind::List(items) => {
                let element_expected = match expected {
                    Some(Type::Slice(inner)) => Some((**inner).clone()),
                    Some(Type::Named { name, args }) if name == "Vec" => args.first().cloned(),
                    _ => None,
                };
                let mut element = element_expected.clone();
                for item in items {
                    let item_ty = self.expr_type(item, element.as_ref());
                    match &element {
                        None => element = Some(item_ty),
                        Some(existing) => {
                            if !item_ty.assignable_to(existing) {
                                self.error(
                                    codes::TYPE_MISMATCH,
                                    item.span,
                                    format!(
                                        "list elements have incompatible types: `{existing}` and `{item_ty}`"
                                    ),
                                );
                            }
                        }
                    }
                }
                Type::Slice(Box::new(element.unwrap_or(Type::Error)))
            }
            ExprKind::Tuple(items) => {
                let expected_items = match expected {
                    Some(Type::Tuple(types)) if types.len() == items.len() => Some(types.clone()),
                    _ => None,
                };
                let types = items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        self.expr_type(item, expected_items.as_ref().map(|t| &t[index]))
                    })
                    .collect();
                Type::Tuple(types)
            }
            ExprKind::StructLit {
                path,
                args,
                fields,
                span,
            } => self.struct_literal_type(path, args, fields, *span, expected),
            ExprKind::MacroCall { name, span, .. } => {
                self.error(
                    codes::MACRO,
                    *span,
                    format!("macro `{name}!` was not expanded"),
                );
                Type::Error
            }
        }
    }

    fn path_type(
        &mut self,
        expr: &Expr,
        path: &[String],
        args: &[TypeExpr],
        expected: Option<&Type>,
    ) -> Type {
        if path.len() == 1 {
            if let Some(var) = self.lookup_var(&path[0]) {
                return var.ty;
            }
        }
        if path.len() == 1 {
            if let Some(ty) = self.env.consts.get(&path[0]) {
                return ty.clone();
            }
            if let Some(function) = self.env.functions.get(&path[0]) {
                return self.function_value_type(&function.sig, args, expected, expr.span);
            }
        }
        let (hint, name) = if path.len() > 1 {
            (Some(path[0].as_str()), path.last().unwrap().as_str())
        } else {
            (None, path[0].as_str())
        };
        if let Some((variant_ty, variant)) = self.env.find_variant(hint, name) {
            let enum_ty =
                self.instantiate_variant(&variant_ty, &variant, args, expected, expr.span);
            if variant.is_unit() {
                return enum_ty;
            }
            if variant.fields.is_empty() {
                let mut params = Vec::new();
                for field_ty in &variant.tuple {
                    params.push(self.substitute_variant_field(&variant_ty, &enum_ty, field_ty));
                }
                return Type::Fn {
                    params,
                    ret: Box::new(enum_ty),
                };
            }
            self.error_note(
                codes::TYPE_MISMATCH,
                expr.span,
                format!("variant `{}` has named fields", variant.name),
                "construct it with `Name { field: value }`",
            );
            return Type::Error;
        }
        // Associated function path: `Type::new`.
        if path.len() == 2 {
            if self.env.types.contains_key(&path[0]) {
                if let Some(method) =
                    self.env
                        .find_method(&Type::named(path[0].clone()), &path[1], &self.generics)
                {
                    return Type::Fn {
                        params: method.params.clone(),
                        ret: Box::new(method.ret.clone()),
                    };
                }
            }
        }
        // Qualified value paths from imported packages.
        if let Some(ty) = self.env.consts.get(name) {
            return ty.clone();
        }
        if let Some(function) = self.env.functions.get(name).cloned() {
            return self.function_value_type(&function.sig, args, expected, expr.span);
        }
        self.error(
            codes::UNKNOWN_NAME,
            expr.span,
            format!("unknown name `{}`", path.join("::")),
        );
        Type::Error
    }

    fn function_value_type(
        &mut self,
        sig: &FnSig,
        args: &[TypeExpr],
        expected: Option<&Type>,
        span: Span,
    ) -> Type {
        if !sig.generics.is_empty() {
            // Referencing a generic function without calling it cannot pick
            // instantiations; require the call form.
            let _ = (args, expected);
            self.error(
                codes::TYPE_MISMATCH,
                span,
                format!(
                    "generic function `{}` must be called with turbofish type arguments",
                    sig.name
                ),
            );
            return Type::Error;
        }
        Type::Fn {
            params: sig.params.clone(),
            ret: Box::new(sig.ret.clone()),
        }
    }

    fn instantiate_variant(
        &mut self,
        variant_ty: &Type,
        variant: &VariantInfo,
        args: &[TypeExpr],
        expected: Option<&Type>,
        span: Span,
    ) -> Type {
        let mut subst = BTreeMap::new();
        // Explicit turbofish.
        let mut explicit = BTreeMap::new();
        if let Type::Named { name, .. } = variant_ty {
            if let Some(info) = self.env.types.get(name) {
                for (generic, arg) in info.generics.iter().zip(args) {
                    explicit.insert(generic.clone(), self.elaborate(arg));
                }
            }
        }
        if !explicit.is_empty() {
            subst.extend(explicit);
        }
        if let Some(expected) = expected {
            unify_types(variant_ty, expected, &mut subst);
        }
        let mut out = variant_ty.clone();
        if let Type::Named { args, .. } = &mut out {
            *args = args
                .iter()
                .map(|arg| match arg {
                    Type::Param(name) => subst
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| Type::Param(name.clone())),
                    other => other.clone(),
                })
                .collect();
        }
        let _ = variant;
        let _ = span;
        out
    }

    fn binary_result(&mut self, op: BinOp, lhs: &Type, rhs: &Type, span: Span) -> Type {
        if lhs.is_error() || rhs.is_error() {
            return Type::Error;
        }
        if op.is_logical() {
            if !lhs.is_bool() {
                self.error(
                    codes::BAD_OPERATOR,
                    span,
                    format!("`{}` expects `bool`, found `{lhs}`", op.symbol()),
                );
            }
            if !rhs.is_bool() {
                self.error(
                    codes::BAD_OPERATOR,
                    span,
                    format!("`{}` expects `bool`, found `{rhs}`", op.symbol()),
                );
            }
            return Type::Bool;
        }
        if op.is_comparison() {
            if matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) {
                let ordered = (lhs.is_numeric() || lhs.is_str() || matches!(lhs, Type::Char))
                    && (rhs.is_numeric() || rhs.is_str() || matches!(rhs, Type::Char));
                if !ordered {
                    self.error(
                        codes::BAD_OPERATOR,
                        span,
                        format!("`{}` cannot compare `{lhs}` and `{rhs}`", op.symbol()),
                    );
                }
            }
            if !lhs.assignable_to(rhs) && !rhs.assignable_to(lhs) {
                self.error(
                    codes::BAD_OPERATOR,
                    span,
                    format!(
                        "`{}` compares values of different types: `{lhs}` and `{rhs}`",
                        op.symbol()
                    ),
                );
            }
            return Type::Bool;
        }
        // Arithmetic and bitwise.
        if op == BinOp::Add && lhs.is_str() && rhs.is_str() {
            return Type::Str;
        }
        if op.is_bitwise() {
            if !lhs.is_integer() || !rhs.is_integer() {
                self.error(
                    codes::BAD_OPERATOR,
                    span,
                    format!(
                        "`{}` expects integer operands, found `{lhs}` and `{rhs}`",
                        op.symbol()
                    ),
                );
                return Type::Error;
            }
            return lhs.clone();
        }
        if !lhs.is_numeric() || !rhs.is_numeric() {
            self.error(
                codes::BAD_OPERATOR,
                span,
                format!(
                    "`{}` expects numeric operands, found `{lhs}` and `{rhs}`",
                    op.symbol()
                ),
            );
            return Type::Error;
        }
        let lhs_int = lhs.is_integer();
        let rhs_int = rhs.is_integer();
        if lhs_int != rhs_int {
            self.error(
                codes::BAD_OPERATOR,
                span,
                format!(
                    "`{}` mixes integer and float operands: `{lhs}` and `{rhs}`",
                    op.symbol()
                ),
            );
            return Type::Error;
        }
        if lhs != rhs {
            // Literal adaptation is handled by calling `expr_type` with an
            // expected type; reaching here means genuinely different types.
            self.error(
                codes::BAD_OPERATOR,
                span,
                format!(
                    "`{}` operands have different types: `{lhs}` and `{rhs}`",
                    op.symbol()
                ),
            );
            return Type::Error;
        }
        lhs.clone()
    }

    fn check_call(
        &mut self,
        expr: &Expr,
        callee: &Expr,
        args: &[Expr],
        expected: Option<&Type>,
    ) -> Type {
        // `Path` callees may be functions, variants, or associated functions.
        if let ExprKind::Path {
            path,
            args: turbofish,
        } = &callee.kind
        {
            if path.len() == 1 {
                if let Some(var) = self.lookup_var(&path[0]) {
                    return self.call_value_type(&var.ty, args, expr.span);
                }
                if let Some(const_ty) = self.env.consts.get(&path[0]) {
                    return self.call_value_type(&const_ty.clone(), args, expr.span);
                }
                if let Some(function) = self.env.functions.get(&path[0]).cloned() {
                    return self.check_function_call(
                        &function.sig,
                        turbofish,
                        args,
                        expected,
                        expr.span,
                    );
                }
            }
            let (hint, name) = if path.len() > 1 {
                (Some(path[0].as_str()), path.last().unwrap().as_str())
            } else {
                (None, path[0].as_str())
            };
            if let Some((variant_ty, variant)) = self.env.find_variant(hint, name) {
                let enum_ty =
                    self.instantiate_variant(&variant_ty, &variant, turbofish, expected, expr.span);
                let param_types: Vec<Type> = if variant.tuple.is_empty() {
                    variant
                        .fields
                        .iter()
                        .map(|f| self.substitute_variant_field(&variant_ty, &enum_ty, &f.ty))
                        .collect()
                } else {
                    variant
                        .tuple
                        .iter()
                        .map(|t| self.substitute_variant_field(&variant_ty, &enum_ty, t))
                        .collect()
                };
                if args.len() != param_types.len() {
                    self.error(
                        codes::WRONG_ARITY,
                        expr.span,
                        format!(
                            "variant `{}` takes {} argument(s), found {}",
                            variant.name,
                            param_types.len(),
                            args.len()
                        ),
                    );
                }
                for (arg, ty) in args.iter().zip(&param_types) {
                    let arg_ty = self.expr_type(arg, Some(ty));
                    if !arg_ty.assignable_to(ty) {
                        self.error(
                            codes::TYPE_MISMATCH,
                            arg.span,
                            format!("expected `{ty}`, found `{arg_ty}`"),
                        );
                    }
                }
                return enum_ty;
            }
            if path.len() == 2 {
                let type_name = &path[0];
                if self.env.types.contains_key(type_name) {
                    if let Some(method) = self.env.find_method(
                        &Type::named(type_name.clone()),
                        &path[1],
                        &self.generics,
                    ) {
                        return self
                            .check_function_call(&method, turbofish, args, expected, expr.span);
                    }
                    if let Some(info) = self.env.types.get(type_name) {
                        if let TypeKind::Struct { .. } = &info.kind {
                            self.error(
                                codes::UNKNOWN_METHOD,
                                expr.span,
                                format!("`{type_name}` has no associated function `{}`", path[1]),
                            );
                            return Type::Error;
                        }
                    }
                }
            }
            // Qualified functions imported from another package:
            // `math::origin` resolves by its final segment.
            if let Some(function) = self.env.functions.get(name).cloned() {
                return self.check_function_call(
                    &function.sig,
                    turbofish,
                    args,
                    expected,
                    expr.span,
                );
            }
            self.error(
                codes::UNKNOWN_NAME,
                expr.span,
                format!("unknown function `{}`", path.join("::")),
            );
            return Type::Error;
        }

        let callee_ty = self.expr_type(callee, None);
        self.call_value_type(&callee_ty, args, expr.span)
    }

    fn call_value_type(&mut self, ty: &Type, args: &[Expr], span: Span) -> Type {
        match ty {
            Type::Fn { params, ret } => {
                if params.len() != args.len() {
                    self.error(
                        codes::WRONG_ARITY,
                        span,
                        format!(
                            "this function takes {} argument(s), found {}",
                            params.len(),
                            args.len()
                        ),
                    );
                }
                for (arg, param) in args.iter().zip(params) {
                    let arg_ty = self.expr_type(arg, Some(param));
                    if !arg_ty.assignable_to(param) {
                        self.error(
                            codes::TYPE_MISMATCH,
                            arg.span,
                            format!("expected `{param}`, found `{arg_ty}`"),
                        );
                    }
                }
                (**ret).clone()
            }
            Type::Param(name) => {
                let bounds = self.generics.bounds.get(name).cloned().unwrap_or_default();
                for bound in &bounds {
                    if let Some((params, ret)) = &bound.fn_sig {
                        if params.len() != args.len() {
                            self.error(
                                codes::WRONG_ARITY,
                                span,
                                format!(
                                    "`{name}: Fn` takes {} argument(s), found {}",
                                    params.len(),
                                    args.len()
                                ),
                            );
                        }
                        for (arg, param) in args.iter().zip(params) {
                            self.expr_type(arg, Some(param));
                        }
                        return ret.clone();
                    }
                }
                self.error(
                    codes::NOT_CALLABLE,
                    span,
                    format!("`{name}` is not callable; add a `Fn(...) -> ...` bound"),
                );
                Type::Error
            }
            Type::Error => Type::Error,
            other => {
                self.error(
                    codes::NOT_CALLABLE,
                    span,
                    format!("value of type `{other}` is not callable"),
                );
                Type::Error
            }
        }
    }

    fn check_function_call(
        &mut self,
        sig: &FnSig,
        turbofish: &[TypeExpr],
        args: &[Expr],
        expected: Option<&Type>,
        span: Span,
    ) -> Type {
        if sig.params.len() != args.len() {
            self.error(
                codes::WRONG_ARITY,
                span,
                format!(
                    "`{}` takes {} argument(s), found {}",
                    sig.name,
                    sig.params.len(),
                    args.len()
                ),
            );
        }
        let mut subst = BTreeMap::new();
        for (generic, explicit) in sig.generics.iter().zip(turbofish) {
            let ty = self.elaborate(explicit);
            subst.insert(generic.name.clone(), ty);
        }
        if let Some(expected) = expected {
            unify_types(&sig.ret, expected, &mut subst);
        }
        let arg_types: Vec<Type> = args
            .iter()
            .map(|arg| {
                if arg_is_int_literal(arg) {
                    Type::Error
                } else {
                    // Do not emit nested errors twice when inferring.
                    self.synth_quiet(arg)
                }
            })
            .collect();
        for (index, param) in sig.params.iter().enumerate() {
            if let Some(arg_ty) = arg_types.get(index) {
                if !arg_ty.is_error() {
                    unify_types(param, arg_ty, &mut subst);
                }
            }
        }
        for generic in &sig.generics {
            if !subst.contains_key(&generic.name) {
                self.error_note(
                    codes::TYPE_MISMATCH,
                    span,
                    format!(
                        "cannot infer type argument `{}` for `{}`",
                        generic.name, sig.name
                    ),
                    "supply it explicitly with turbofish, e.g. `f::<i32>(...)`",
                );
            }
        }
        let params: Vec<Type> = sig
            .params
            .iter()
            .map(|param| param.substitute(&subst))
            .collect();
        for (index, arg) in args.iter().enumerate() {
            if let Some(param) = params.get(index) {
                let arg_ty = self.expr_type(arg, Some(param));
                if !arg_ty.assignable_to(param) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        arg.span,
                        format!("expected `{param}`, found `{arg_ty}`"),
                    );
                }
            }
        }
        for generic in &sig.generics {
            if let Some(arg_ty) = subst.get(&generic.name) {
                self.check_bound_satisfaction(generic, arg_ty, span);
            }
        }
        let _ = arg_types;
        sig.ret.substitute(&subst)
    }

    fn check_method_call(
        &mut self,
        _expr: &Expr,
        receiver: &Expr,
        name: &str,
        args: &[Expr],
        span: Span,
        expected: Option<&Type>,
    ) -> Type {
        let receiver_ty = self.expr_type(receiver, None);
        let Some(sig) = self.env.find_method(&receiver_ty, name, &self.generics) else {
            self.error(
                codes::UNKNOWN_METHOD,
                span,
                format!("no method `{name}` for type `{receiver_ty}`"),
            );
            return Type::Error;
        };
        // Drop the receiver parameter; it is supplied by the method call.
        let mut call_sig = sig.clone();
        if call_sig.has_self() {
            call_sig.params.remove(0);
            call_sig.param_names.remove(0);
        }
        self.check_function_call(&call_sig, &[], args, expected, span)
    }

    fn field_type(&mut self, ty: &Type, name: &str, span: Span) -> Type {
        let ty = ty.deref().clone();
        if let Some(fields) = self.env.struct_fields(&ty) {
            if let Some((_, field_ty)) = fields.iter().find(|(n, _)| n == name) {
                return field_ty.clone();
            }
        }
        if let Type::Tuple(items) = &ty {
            if let Ok(index) = name.parse::<usize>() {
                if let Some(item) = items.get(index) {
                    return item.clone();
                }
            }
        }
        match &ty {
            Type::Param(param) => {
                self.error(
                    codes::UNKNOWN_FIELD,
                    span,
                    format!(
                        "cannot access field `{name}` on type parameter `{param}`; fields are not part of bounds"
                    ),
                );
            }
            Type::Error => {}
            _ => self.error(
                codes::UNKNOWN_FIELD,
                span,
                format!("no field `{name}` on type `{ty}`"),
            ),
        }
        Type::Error
    }

    fn try_type(&mut self, expr: &Expr, inner: &Expr) -> Type {
        let carrier = self.ret.clone();
        match &carrier {
            Type::Named { name, args } if name == "Result" && args.len() == 2 => {
                let error_target = args[1].clone();
                let ok_type = args[0].clone();
                let inner_ty = self.expr_type(inner, None);
                match &inner_ty {
                    Type::Named {
                        name: inner_name,
                        args: inner_args,
                    } if inner_name == "Result" && inner_args.len() == 2 => {
                        let from = &inner_args[1];
                        if !from.assignable_to(&error_target)
                            && !self.env.from_impl_exists(from, &error_target)
                        {
                            self.error_note(
                                codes::QUESTION_MARK,
                                expr.span,
                                format!(
                                    "`?` cannot convert error type `{from}` into `{error_target}`"
                                ),
                                format!("implement `From<{from}> for {error_target}`"),
                            );
                        }
                        let _ = ok_type;
                        inner_args[0].clone()
                    }
                    Type::Error => Type::Error,
                    other => {
                        self.error_note(
                            codes::QUESTION_MARK,
                            expr.span,
                            format!("`?` expects a `Result`, found `{other}`"),
                            format!("the enclosing function returns `{carrier}`"),
                        );
                        Type::Error
                    }
                }
            }
            Type::Named { name, args } if name == "Option" && args.len() == 1 => {
                let inner_ty = self.expr_type(inner, None);
                match &inner_ty {
                    Type::Named {
                        name: inner_name,
                        args: inner_args,
                    } if inner_name == "Option" && inner_args.len() == 1 => inner_args[0].clone(),
                    Type::Error => Type::Error,
                    other => {
                        self.error_note(
                            codes::QUESTION_MARK,
                            expr.span,
                            format!("`?` expects an `Option`, found `{other}`"),
                            format!("the enclosing function returns `{carrier}`"),
                        );
                        Type::Error
                    }
                }
            }
            _ => {
                self.error_note(
                    codes::QUESTION_MARK,
                    expr.span,
                    "`?` requires the enclosing function to return `Result<T, E>` or `Option<T>`",
                    format!("this function returns `{carrier}`"),
                );
                Type::Error
            }
        }
    }

    fn struct_literal_type(
        &mut self,
        path: &[String],
        args: &[TypeExpr],
        fields: &[(String, Expr)],
        span: Span,
        expected: Option<&Type>,
    ) -> Type {
        let hint = if path.len() > 1 {
            Some(path[0].as_str())
        } else {
            None
        };
        let name = path.last().cloned().unwrap_or_default();

        // Enum variant with named fields.
        if let Some((variant_ty, variant)) = self.env.find_variant(hint, &name) {
            if variant.fields.is_empty() {
                self.error(
                    codes::TYPE_MISMATCH,
                    span,
                    format!("variant `{}` has no named fields", variant.name),
                );
                return Type::Error;
            }
            let enum_ty = self.instantiate_variant(&variant_ty, &variant, args, expected, span);
            let mut seen = BTreeSet::new();
            for (field_name, value) in fields {
                let Some(field) = variant.fields.iter().find(|f| &f.name == field_name) else {
                    self.error(
                        codes::UNKNOWN_FIELD,
                        value.span,
                        format!("variant `{}` has no field `{field_name}`", variant.name),
                    );
                    continue;
                };
                seen.insert(field_name.clone());
                let field_ty = self.substitute_variant_field(&variant_ty, &enum_ty, &field.ty);
                let value_ty = self.expr_type(value, Some(&field_ty));
                if !value_ty.assignable_to(&field_ty) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        value.span,
                        format!("field `{field_name}` expects `{field_ty}`, found `{value_ty}`"),
                    );
                }
            }
            for field in &variant.fields {
                if !seen.contains(&field.name) {
                    self.error(
                        codes::MISSING_FIELD,
                        span,
                        format!("missing field `{}` in `{}`", field.name, variant.name),
                    );
                }
            }
            return enum_ty;
        }

        let Some(info) = self.env.types.get(&name) else {
            self.error(
                codes::UNKNOWN_TYPE,
                span,
                format!("unknown struct `{name}`"),
            );
            return Type::Error;
        };
        let TypeKind::Struct { fields: def_fields } = &info.kind else {
            self.error(
                codes::TYPE_MISMATCH,
                span,
                format!("`{name}` is not a struct"),
            );
            return Type::Error;
        };
        let mut subst = BTreeMap::new();
        for (generic, explicit) in info.generics.iter().zip(args) {
            subst.insert(generic.clone(), self.elaborate(explicit));
        }
        if let Some(expected) = expected {
            unify_types(&info.as_type(), expected, &mut subst);
        }
        let struct_ty = Type::Named {
            name: name.clone(),
            args: info
                .generics
                .iter()
                .map(|g| {
                    subst
                        .get(g)
                        .cloned()
                        .unwrap_or_else(|| Type::Param(g.clone()))
                })
                .collect(),
        };
        let mut seen = BTreeSet::new();
        for (field_name, value) in fields {
            let Some(field) = def_fields.iter().find(|f| &f.name == field_name) else {
                self.error(
                    codes::UNKNOWN_FIELD,
                    value.span,
                    format!("`{name}` has no field `{field_name}`"),
                );
                continue;
            };
            seen.insert(field_name.clone());
            let field_ty = field.ty.substitute(&subst);
            let value_ty = self.expr_type(value, Some(&field_ty));
            if !value_ty.assignable_to(&field_ty) {
                self.error(
                    codes::TYPE_MISMATCH,
                    value.span,
                    format!("field `{field_name}` expects `{field_ty}`, found `{value_ty}`"),
                );
            }
        }
        for field in def_fields {
            if !seen.contains(&field.name) {
                self.error(
                    codes::MISSING_FIELD,
                    span,
                    format!("missing field `{}` in `{name}`", field.name),
                );
            }
        }
        struct_ty
    }

    fn check_bound_satisfaction(&mut self, generic: &GenericInfo, arg: &Type, span: Span) {
        if arg.is_error() {
            return;
        }
        for bound in &generic.bounds {
            if bound.fn_sig.is_some() || bound.iface == "Fn" {
                let is_fn = matches!(arg, Type::Fn { .. });
                let param_ok = match arg {
                    Type::Param(param) => self
                        .generics
                        .bounds
                        .get(param)
                        .map(|bounds| bounds.iter().any(|b| b.fn_sig.is_some() || b.iface == "Fn"))
                        .unwrap_or(false),
                    _ => false,
                };
                if !is_fn && !param_ok {
                    self.error(
                        codes::BOUND_NOT_SATISFIED,
                        span,
                        format!("`{arg}` does not satisfy `{}: Fn(...)`", generic.name),
                    );
                }
                continue;
            }
            if let Type::Param(param) = arg {
                let implied = self
                    .generics
                    .bounds
                    .get(param)
                    .map(|bounds| bounds.iter().any(|b| b.iface == bound.iface))
                    .unwrap_or(false);
                if !implied {
                    self.error(
                        codes::BOUND_NOT_SATISFIED,
                        span,
                        format!(
                            "type parameter `{param}` is not known to implement `{}`",
                            bound.iface
                        ),
                    );
                }
                continue;
            }
            if !self.env.impl_exists(&bound.iface, arg, &bound.args) {
                self.error_note(
                    codes::BOUND_NOT_SATISFIED,
                    span,
                    format!("`{arg}` does not implement `{}`", bound.iface),
                    format!(
                        "add `impl {} for {arg}` in the package that defines `{arg}`",
                        bound.iface
                    ),
                );
            }
        }
    }

    /// Synthesize a type without reporting errors. Used while inferring call
    /// arguments so a failure is reported once, by the definite check.
    fn synth_quiet(&mut self, expr: &Expr) -> Type {
        let saved = std::mem::take(&mut self.errors);
        let ty = self.expr_type(expr, None);
        self.errors = saved;
        ty
    }

    // ---------------------------------------------------------- exhaustiveness

    fn check_exhaustiveness(&mut self, ty: &Type, arms: &[MatchArm], span: Span) {
        let rows: Vec<&Pattern> = arms
            .iter()
            .filter(|arm| arm.guard.is_none())
            .flat_map(|arm| match &arm.pattern.kind {
                PatKind::Or(alternatives) => alternatives.iter().collect::<Vec<_>>(),
                _ => vec![&arm.pattern],
            })
            .collect();
        if rows.iter().any(|p| p.is_irrefutable()) {
            return;
        }
        if self.matrix_exhaustive(
            &[ty.clone()],
            &rows.iter().map(|p| vec![*p]).collect::<Vec<_>>(),
        ) {
            return;
        }
        let missing = self.missing_constructors(ty, &rows);
        let message = if missing.is_empty() {
            format!("non-exhaustive `match` on `{ty}`")
        } else {
            format!(
                "non-exhaustive `match` on `{ty}`; missing {}",
                missing.join(", ")
            )
        };
        self.error_note(
            codes::NON_EXHAUSTIVE,
            span,
            message,
            "add the missing patterns or a `_ =>` arm",
        );
    }

    /// Matrix-based exhaustiveness over closed constructors. Rows are pattern
    /// vectors of equal length; the first column is specialized per
    /// constructor (a decision-tree-free usefulness approximation).
    fn matrix_exhaustive(&self, types: &[Type], rows: &[Vec<&Pattern>]) -> bool {
        if types.is_empty() {
            return !rows.is_empty();
        }
        if rows.is_empty() {
            return false;
        }
        let ty = types[0].deref();
        let wildcard_column = rows.iter().any(|row| row[0].is_irrefutable());
        if types.len() == 1 && wildcard_column {
            return true;
        }

        if let Some((info, _)) = self.env.enum_info(ty) {
            if let TypeKind::Enum { open: true, .. } = &info.kind {
                if !wildcard_column {
                    // Open enums require an explicit `_` arm.
                    return false;
                }
            }
        }

        let Some(constructors) = self.constructors(ty) else {
            if wildcard_column {
                let rest: Vec<Vec<&Pattern>> = rows.iter().map(|row| row[1..].to_vec()).collect();
                return self.matrix_exhaustive(&types[1..], &rest);
            }
            return false;
        };

        for constructor in constructors {
            let arity = constructor.arity();
            let mut sub_rows: Vec<Vec<&Pattern>> = Vec::new();
            for row in rows {
                if row[0].is_irrefutable() {
                    let mut next = vec![wildcard_pattern(); arity];
                    next.extend(row[1..].iter().copied());
                    sub_rows.push(next);
                    continue;
                }
                if let Some(mut sub) = specialize(row[0], ty, &constructor) {
                    while sub.len() < arity {
                        sub.push(wildcard_pattern());
                    }
                    let mut next = sub;
                    next.extend(row[1..].iter().copied());
                    sub_rows.push(next);
                }
            }
            if sub_rows.is_empty() {
                return false;
            }
            let mut rec_types = constructor.field_types();
            rec_types.extend(types[1..].iter().cloned());
            if !self.matrix_exhaustive(&rec_types, &sub_rows) {
                return false;
            }
        }
        true
    }

    fn constructors(&self, ty: &Type) -> Option<Vec<Ctor>> {
        match ty {
            Type::Bool => Some(vec![Ctor::Bool(true), Ctor::Bool(false)]),
            Type::Named { .. } => {
                if let Some((info, args)) = self.env.enum_info(ty) {
                    if let TypeKind::Enum { variants, .. } = &info.kind {
                        let subst: BTreeMap<String, Type> = info
                            .generics
                            .iter()
                            .cloned()
                            .zip(args.iter().cloned())
                            .collect();
                        return Some(
                            variants
                                .iter()
                                .map(|variant| {
                                    if variant.tuple.is_empty() {
                                        Ctor::Enum {
                                            name: variant.name.clone(),
                                            fields: variant
                                                .fields
                                                .iter()
                                                .map(|f| (f.name.clone(), f.ty.substitute(&subst)))
                                                .collect(),
                                        }
                                    } else {
                                        Ctor::Enum {
                                            name: variant.name.clone(),
                                            fields: variant
                                                .tuple
                                                .iter()
                                                .enumerate()
                                                .map(|(i, t)| (i.to_string(), t.substitute(&subst)))
                                                .collect(),
                                        }
                                    }
                                })
                                .collect(),
                        );
                    }
                }
                if let Some(fields) = self.env.struct_fields(ty) {
                    return Some(vec![Ctor::Struct { fields }]);
                }
                None
            }
            Type::Tuple(items) => Some(vec![Ctor::Tuple {
                fields: items.clone(),
            }]),
            _ => None,
        }
    }

    fn missing_constructors(&self, ty: &Type, rows: &[&Pattern]) -> Vec<String> {
        match ty.deref() {
            Type::Bool => {
                let mut missing = Vec::new();
                let has = |want: bool| {
                    rows.iter().any(|row| {
                        matches!(&row.kind, PatKind::Literal(PatLiteral::Bool(v)) if *v == want)
                    })
                };
                if !has(true) {
                    missing.push("`true`".to_string());
                }
                if !has(false) {
                    missing.push("`false`".to_string());
                }
                missing
            }
            Type::Named { .. } => {
                if let Some((info, _)) = self.env.enum_info(ty) {
                    if let TypeKind::Enum { variants, open } = &info.kind {
                        if *open {
                            return vec!["_".into()];
                        }
                        let mut missing = Vec::new();
                        for variant in variants {
                            let covered = rows.iter().any(|row| match &row.kind {
                                PatKind::Path { path, .. } | PatKind::Struct { path, .. } => {
                                    path.last().map(String::as_str) == Some(&variant.name)
                                }
                                _ => false,
                            });
                            if !covered {
                                missing.push(format!("`{}`", variant.name));
                            }
                        }
                        return missing;
                    }
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }
}

enum Ctor {
    Enum {
        name: String,
        fields: Vec<(String, Type)>,
    },
    Struct {
        fields: Vec<(String, Type)>,
    },
    Tuple {
        fields: Vec<Type>,
    },
    Bool(bool),
}

impl Ctor {
    fn arity(&self) -> usize {
        match self {
            Ctor::Enum { fields, .. } | Ctor::Struct { fields } => fields.len(),
            Ctor::Tuple { fields } => fields.len(),
            Ctor::Bool(_) => 0,
        }
    }

    fn field_types(&self) -> Vec<Type> {
        match self {
            Ctor::Enum { fields, .. } | Ctor::Struct { fields } => {
                fields.iter().map(|(_, ty)| ty.clone()).collect()
            }
            Ctor::Tuple { fields } => fields.clone(),
            Ctor::Bool(_) => Vec::new(),
        }
    }
}

fn specialize<'a>(pattern: &'a Pattern, ty: &Type, constructor: &Ctor) -> Option<Vec<&'a Pattern>> {
    match constructor {
        Ctor::Bool(value) => match &pattern.kind {
            PatKind::Literal(PatLiteral::Bool(found)) if found == value => Some(Vec::new()),
            _ => None,
        },
        Ctor::Tuple { fields } => match &pattern.kind {
            PatKind::Tuple(items) if items.len() <= fields.len() => {
                let mut out: Vec<&Pattern> = items.iter().collect();
                while out.len() < fields.len() {
                    out.push(wildcard_pattern());
                }
                Some(out)
            }
            _ => None,
        },
        Ctor::Struct { fields } => match &pattern.kind {
            PatKind::Struct {
                path, fields: pats, ..
            } => {
                if path.last().map(String::as_str) != Some(&ty.head()) {
                    return None;
                }
                Some(order_fields(fields, pats))
            }
            _ => None,
        },
        Ctor::Enum { name, fields } => match &pattern.kind {
            PatKind::Path { path, args } => {
                if path.last().map(String::as_str) != Some(name.as_str()) {
                    return None;
                }
                let mut out: Vec<&Pattern> = args.iter().collect();
                while out.len() < fields.len() {
                    out.push(wildcard_pattern());
                }
                Some(out)
            }
            PatKind::Struct {
                path, fields: pats, ..
            } => {
                if path.last().map(String::as_str) != Some(name.as_str()) {
                    return None;
                }
                Some(order_fields(fields, pats))
            }
            _ => None,
        },
    }
}

fn order_fields<'a>(
    fields: &[(String, Type)],
    patterns: &'a [(String, Pattern)],
) -> Vec<&'a Pattern> {
    let mut out = Vec::new();
    for (name, _) in fields {
        let mut found: Option<&'a Pattern> = None;
        for (field, pattern) in patterns {
            if field == name {
                found = Some(pattern);
                break;
            }
        }
        match found {
            Some(pattern) => out.push(pattern),
            None => out.push(wildcard_pattern()),
        }
    }
    out
}

fn wildcard_pattern() -> &'static Pattern {
    static WILDCARD: std::sync::OnceLock<Pattern> = std::sync::OnceLock::new();
    WILDCARD.get_or_init(|| Pattern::new(PatKind::Wildcard, Span::dummy()))
}

fn assignment_root(expr: &Expr) -> Option<(String, Span)> {
    match &expr.kind {
        ExprKind::Path { path, .. } if path.len() == 1 => Some((path[0].clone(), expr.span)),
        ExprKind::Field { base, .. } | ExprKind::Index { base, .. } => assignment_root(base),
        _ => None,
    }
}

fn block_diverges(block: &Block) -> bool {
    block.stmts.iter().any(|stmt| match &stmt.kind {
        StmtKind::Return(_) | StmtKind::Break | StmtKind::Continue => true,
        StmtKind::Expr(expr) => expr_diverges(expr),
        _ => false,
    })
}

fn expr_diverges(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Call { callee, .. } => match &callee.kind {
            ExprKind::Path { path, .. } => path.last().map(String::as_str) == Some("panic_impl"),
            _ => false,
        },
        ExprKind::Block(block) => block_diverges(block),
        _ => false,
    }
}

fn literal_type(literal: &PatLiteral) -> Type {
    match literal {
        PatLiteral::Int(_, _) => Type::Int(IntTy::I32),
        PatLiteral::Float(_, _) => Type::Float(crate::types::FloatTy::F64),
        PatLiteral::Str(_) => Type::Str,
        PatLiteral::Char(_) => Type::Char,
        PatLiteral::Bool(_) => Type::Bool,
    }
}

fn literal_matches(literal: &Type, target: &Type) -> bool {
    if target.is_error() {
        return true;
    }
    if literal == target {
        return true;
    }
    // Integer and float literals adapt to any numeric type of the same kind.
    if literal.is_integer() && target.is_integer() {
        return true;
    }
    if literal.is_float() && target.is_float() {
        return true;
    }
    false
}

fn arg_is_int_literal(expr: &Expr) -> bool {
    matches!(expr.kind, ExprKind::Int { .. })
}

fn unify_types(pattern: &Type, concrete: &Type, subst: &mut BTreeMap<String, Type>) -> bool {
    if pattern.is_error() || concrete.is_error() {
        return true;
    }
    match pattern {
        Type::Param(name) => {
            if let Some(existing) = subst.get(name) {
                existing == concrete
            } else {
                subst.insert(name.clone(), concrete.clone());
                true
            }
        }
        Type::Named {
            name: pname,
            args: pargs,
        } => match concrete {
            Type::Named { name, args } if name == pname && args.len() == pargs.len() => args
                .iter()
                .zip(pargs)
                .all(|(a, p)| unify_types(p, a, subst)),
            _ => false,
        },
        Type::Ref {
            inner: pinner,
            mutable: pmut,
        } => match concrete {
            Type::Ref { inner, mutable } if mutable == pmut => unify_types(pinner, inner, subst),
            _ => false,
        },
        Type::Slice(pinner) => match concrete {
            Type::Slice(inner) => unify_types(pinner, inner, subst),
            _ => false,
        },
        Type::Tuple(pitems) => match concrete {
            Type::Tuple(items) if items.len() == pitems.len() => items
                .iter()
                .zip(pitems)
                .all(|(a, p)| unify_types(p, a, subst)),
            _ => false,
        },
        Type::Fn { params, ret } => match concrete {
            Type::Fn {
                params: cparams,
                ret: cret,
            } if params.len() == cparams.len() => {
                params
                    .iter()
                    .zip(cparams)
                    .all(|(p, c)| unify_types(p, c, subst))
                    && unify_types(ret, cret, subst)
            }
            _ => false,
        },
        Type::SelfType => true,
        other => other == concrete,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_env_has_prelude() {
        let env = Env::builtin();
        assert!(env.types.contains_key("Option"));
        assert!(env.types.contains_key("Result"));
        assert!(env.interfaces.contains_key("Iterator"));
    }

    #[test]
    fn from_lookup_is_by_head() {
        let env = Env::builtin();
        assert!(!env.from_impl_exists(&Type::named("A"), &Type::named("B")));
    }
}
