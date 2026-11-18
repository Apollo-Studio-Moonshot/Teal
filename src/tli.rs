//! The `.tli` interface summary.
//!
//! One deterministic binary file per package. It is the only thing a downstream
//! package reads about a dependency (DESIGN.md §4.2). Every digest is a Merkle
//! hash over a canonical serialization, so signature changes propagate through
//! fingerprints while body-only changes do not.

use crate::checker::{check_package, Env, FnSig, TypeKind};
use crate::hash::{self, Digest};
use crate::macros::{macro_digest, render_tokens};
use crate::types::{parse_type, Type};
use crate::{COMPILER_ABI, COMPILER_VERSION, FORMAT_VERSION};
use std::collections::BTreeMap;

pub const MAGIC: &[u8; 4] = b"TLI\0";

#[derive(Clone, Debug)]
pub struct TliType {
    /// "struct", "enum", "opaque", or "alias".
    pub kind: String,
    pub name: String,
    pub generics: Vec<String>,
    pub fields: Vec<(String, String)>,
    pub variants: Vec<TliVariant>,
    pub open: bool,
    pub alias: Option<String>,
    pub repr: Option<String>,
}

#[derive(Clone, Debug)]
pub struct TliVariant {
    pub name: String,
    pub fields: Vec<(String, String)>,
    pub tuple: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct TliMethod {
    pub name: String,
    pub generics: Vec<String>,
    pub param_names: Vec<String>,
    pub params: Vec<String>,
    pub ret: String,
    pub has_body: bool,
}

impl TliMethod {
    pub fn to_sig(&self) -> FnSig {
        FnSig {
            name: self.name.clone(),
            generics: self
                .generics
                .iter()
                .map(|name| crate::checker::GenericInfo {
                    name: name.clone(),
                    bounds: Vec::new(),
                })
                .collect(),
            params: self
                .params
                .iter()
                .map(|t| parse_type(t).unwrap_or(Type::Error))
                .collect(),
            param_names: self.param_names.clone(),
            ret: parse_type(&self.ret).unwrap_or(Type::Error),
        }
    }
}

#[derive(Clone, Debug)]
pub struct TliInterface {
    pub name: String,
    pub generics: Vec<String>,
    pub assoc: Vec<String>,
    pub methods: Vec<TliMethod>,
}

#[derive(Clone, Debug)]
pub struct TliImpl {
    pub self_ty: String,
    pub iface: Option<String>,
    pub iface_args: Vec<String>,
    pub assoc: Vec<(String, String)>,
    pub generics: Vec<String>,
    pub methods: Vec<TliMethod>,
}

#[derive(Clone, Debug)]
pub struct TliFunction {
    pub name: String,
    pub generics: Vec<String>,
    pub params: Vec<String>,
    pub param_names: Vec<String>,
    pub ret: String,
}

impl TliFunction {
    pub fn to_sig(&self) -> FnSig {
        FnSig {
            name: self.name.clone(),
            generics: self
                .generics
                .iter()
                .map(|name| crate::checker::GenericInfo {
                    name: name.clone(),
                    bounds: Vec::new(),
                })
                .collect(),
            params: self
                .params
                .iter()
                .map(|t| parse_type(t).unwrap_or(Type::Error))
                .collect(),
            param_names: self.param_names.clone(),
            ret: parse_type(&self.ret).unwrap_or(Type::Error),
        }
    }
}

#[derive(Clone, Debug)]
pub struct TliMacro {
    pub name: String,
    pub rules: Vec<(String, String)>,
    pub digest: Digest,
}

#[derive(Clone, Debug)]
pub struct Tli {
    pub format_version: u32,
    pub abi: String,
    pub compiler_version: String,
    pub package: String,
    pub content_hash: Digest,
    pub fingerprint: Digest,
    pub builtin: bool,
    pub deps: Vec<(String, Digest)>,
    pub types: Vec<TliType>,
    pub interfaces: Vec<TliInterface>,
    pub impls: Vec<TliImpl>,
    pub functions: Vec<TliFunction>,
    pub consts: Vec<(String, String)>,
    pub macros: Vec<TliMacro>,
    pub instances: Vec<String>,
    pub docs: Vec<(String, String)>,
}

impl Tli {
    pub fn package_names(&self) -> impl Iterator<Item = &String> {
        self.deps.iter().map(|(name, _)| name)
    }

    /// Canonical bytes used for the content hash. Hashes themselves are
    /// excluded so the hash is a pure function of exported summaries.
    fn canonical_bytes(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.str(&self.abi);
        w.str(&self.compiler_version);
        w.str(&self.package);
        w.bool(self.builtin);
        w.list(&self.deps, |w, (name, hash)| {
            w.str(name);
            w.digest(hash);
        });
        w.list(&self.types, |w, ty| {
            w.str(&ty.kind);
            w.str(&ty.name);
            w.list(&ty.generics, |w, g| w.str(g));
            w.list(&ty.fields, |w, (name, ty)| {
                w.str(name);
                w.str(ty);
            });
            w.list(&ty.variants, |w, variant| {
                w.str(&variant.name);
                w.list(&variant.fields, |w, (name, ty)| {
                    w.str(name);
                    w.str(ty);
                });
                w.list(&variant.tuple, |w, ty| w.str(ty));
            });
            w.bool(ty.open);
            w.opt(&ty.alias, |w, value| w.str(value));
            w.opt(&ty.repr, |w, value| w.str(value));
        });
        w.list(&self.interfaces, |w, iface| {
            w.str(&iface.name);
            w.list(&iface.generics, |w, g| w.str(g));
            w.list(&iface.assoc, |w, a| w.str(a));
            w.list(&iface.methods, write_method);
        });
        w.list(&self.impls, |w, imp| {
            w.str(&imp.self_ty);
            w.opt(&imp.iface, |w, value| w.str(value));
            w.list(&imp.iface_args, |w, t| w.str(t));
            w.list(&imp.assoc, |w, (k, v)| {
                w.str(k);
                w.str(v);
            });
            w.list(&imp.generics, |w, g| w.str(g));
            w.list(&imp.methods, write_method);
        });
        w.list(&self.functions, |w, f| {
            w.str(&f.name);
            w.list(&f.generics, |w, g| w.str(g));
            w.list(&f.params, |w, t| w.str(t));
            w.list(&f.param_names, |w, n| w.str(n));
            w.str(&f.ret);
        });
        w.list(&self.consts, |w, (name, ty)| {
            w.str(name);
            w.str(ty);
        });
        w.list(&self.macros, |w, m| {
            w.str(&m.name);
            w.digest(&m.digest);
            w.list(&m.rules, |w, (matcher, transcriber)| {
                w.str(matcher);
                w.str(transcriber);
            });
        });
        w.list(&self.instances, |w, i| w.str(i));
        w.list(&self.docs, |w, (k, v)| {
            w.str(k);
            w.str(v);
        });
        w.buf
    }

    pub fn compute_content_hash(&self) -> Digest {
        hash::sha256(&self.canonical_bytes())
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.raw(MAGIC);
        w.u32(self.format_version);
        w.str(&self.abi);
        w.str(&self.compiler_version);
        w.str(&self.package);
        w.digest(&self.content_hash);
        w.digest(&self.fingerprint);
        w.bool(self.builtin);
        w.list(&self.deps, |w, (name, hash)| {
            w.str(name);
            w.digest(hash);
        });
        w.list(&self.types, |w, ty| {
            w.str(&ty.kind);
            w.str(&ty.name);
            w.list(&ty.generics, |w, g| w.str(g));
            w.list(&ty.fields, |w, (name, ty)| {
                w.str(name);
                w.str(ty);
            });
            w.list(&ty.variants, |w, variant| {
                w.str(&variant.name);
                w.list(&variant.fields, |w, (name, ty)| {
                    w.str(name);
                    w.str(ty);
                });
                w.list(&variant.tuple, |w, ty| w.str(ty));
            });
            w.bool(ty.open);
            w.opt(&ty.alias, |w, value| w.str(value));
            w.opt(&ty.repr, |w, value| w.str(value));
        });
        w.list(&self.interfaces, |w, iface| {
            w.str(&iface.name);
            w.list(&iface.generics, |w, g| w.str(g));
            w.list(&iface.assoc, |w, a| w.str(a));
            w.list(&iface.methods, write_method);
        });
        w.list(&self.impls, |w, imp| {
            w.str(&imp.self_ty);
            w.opt(&imp.iface, |w, value| w.str(value));
            w.list(&imp.iface_args, |w, t| w.str(t));
            w.list(&imp.assoc, |w, (k, v)| {
                w.str(k);
                w.str(v);
            });
            w.list(&imp.generics, |w, g| w.str(g));
            w.list(&imp.methods, write_method);
        });
        w.list(&self.functions, |w, f| {
            w.str(&f.name);
            w.list(&f.generics, |w, g| w.str(g));
            w.list(&f.params, |w, t| w.str(t));
            w.list(&f.param_names, |w, n| w.str(n));
            w.str(&f.ret);
        });
        w.list(&self.consts, |w, (name, ty)| {
            w.str(name);
            w.str(ty);
        });
        w.list(&self.macros, |w, m| {
            w.str(&m.name);
            w.digest(&m.digest);
            w.list(&m.rules, |w, (matcher, transcriber)| {
                w.str(matcher);
                w.str(transcriber);
            });
        });
        w.list(&self.instances, |w, i| w.str(i));
        w.list(&self.docs, |w, (k, v)| {
            w.str(k);
            w.str(v);
        });
        w.buf
    }

    pub fn decode(bytes: &[u8]) -> Result<Tli, String> {
        let mut r = Reader::new(bytes);
        let magic = r.take(4)?;
        if magic != MAGIC {
            return Err("not a Teal interface file (bad magic)".into());
        }
        let format_version = r.u32()?;
        if format_version != FORMAT_VERSION {
            return Err(format!(
                "interface format {} is not supported (this compiler emits {})",
                format_version, FORMAT_VERSION
            ));
        }
        let abi = r.str()?;
        if abi != COMPILER_ABI {
            return Err(format!(
                "compiler ABI mismatch: artifact uses `{abi}`, compiler is `{COMPILER_ABI}`"
            ));
        }
        let compiler_version = r.str()?;
        let package = r.str()?;
        let content_hash = r.digest()?;
        let fingerprint = r.digest()?;
        let builtin = r.bool()?;
        let deps = r.list(|r| Ok((r.str()?, r.digest()?)))?;
        let types = r.list(|r| {
            Ok(TliType {
                kind: r.str()?,
                name: r.str()?,
                generics: r.list(|r| r.str())?,
                fields: r.list(|r| Ok((r.str()?, r.str()?)))?,
                variants: r.list(|r| {
                    Ok(TliVariant {
                        name: r.str()?,
                        fields: r.list(|r| Ok((r.str()?, r.str()?)))?,
                        tuple: r.list(|r| r.str())?,
                    })
                })?,
                open: r.bool()?,
                alias: r.opt(|r| r.str())?,
                repr: r.opt(|r| r.str())?,
            })
        })?;
        let interfaces = r.list(|r| {
            Ok(TliInterface {
                name: r.str()?,
                generics: r.list(|r| r.str())?,
                assoc: r.list(|r| r.str())?,
                methods: r.list(read_method)?,
            })
        })?;
        let impls = r.list(|r| {
            Ok(TliImpl {
                self_ty: r.str()?,
                iface: r.opt(|r| r.str())?,
                iface_args: r.list(|r| r.str())?,
                assoc: r.list(|r| Ok((r.str()?, r.str()?)))?,
                generics: r.list(|r| r.str())?,
                methods: r.list(read_method)?,
            })
        })?;
        let functions = r.list(|r| {
            Ok(TliFunction {
                name: r.str()?,
                generics: r.list(|r| r.str())?,
                params: r.list(|r| r.str())?,
                param_names: r.list(|r| r.str())?,
                ret: r.str()?,
            })
        })?;
        let consts = r.list(|r| Ok((r.str()?, r.str()?)))?;
        let macros = r.list(|r| {
            Ok(TliMacro {
                name: r.str()?,
                digest: r.digest()?,
                rules: r.list(|r| Ok((r.str()?, r.str()?)))?,
            })
        })?;
        let instances = r.list(|r| r.str())?;
        let docs = r.list(|r| Ok((r.str()?, r.str()?)))?;
        if !r.at_end() {
            return Err("trailing bytes in interface file".into());
        }
        Ok(Tli {
            format_version,
            abi,
            compiler_version,
            package,
            content_hash,
            fingerprint,
            builtin,
            deps,
            types,
            interfaces,
            impls,
            functions,
            consts,
            macros,
            instances,
            docs,
        })
    }

    pub fn docs_for(&self, key: &str) -> Option<&str> {
        self.docs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

fn write_method(w: &mut Writer, method: &TliMethod) {
    w.str(&method.name);
    w.list(&method.generics, |w, g| w.str(g));
    w.list(&method.param_names, |w, n| w.str(n));
    w.list(&method.params, |w, t| w.str(t));
    w.str(&method.ret);
    w.bool(method.has_body);
}

fn read_method(r: &mut Reader<'_>) -> Result<TliMethod, String> {
    Ok(TliMethod {
        name: r.str()?,
        generics: r.list(|r| r.str())?,
        param_names: r.list(|r| r.str())?,
        params: r.list(|r| r.str())?,
        ret: r.str()?,
        has_body: r.bool()?,
    })
}

/// Build a summary from a checked environment. Only the current package's
/// public surface is exported; impls are always exported because conformance
/// must be visible across packages.
pub fn emit(env: &Env, deps: Vec<(String, Digest)>, fingerprint: Digest) -> Tli {
    let mut types = Vec::new();
    let mut docs = Vec::new();
    for info in env.types.values() {
        if info.package != env.package || !info.public {
            continue;
        }
        let (kind, fields, variants, open, alias, repr) = match &info.kind {
            TypeKind::Struct { fields } => (
                "struct".to_string(),
                fields
                    .iter()
                    .map(|f| (f.name.clone(), f.ty.to_string()))
                    .collect(),
                Vec::new(),
                false,
                None,
                None,
            ),
            TypeKind::Enum { variants, open } => (
                "enum".to_string(),
                Vec::new(),
                variants
                    .iter()
                    .map(|v| TliVariant {
                        name: v.name.clone(),
                        fields: v
                            .fields
                            .iter()
                            .map(|f| (f.name.clone(), f.ty.to_string()))
                            .collect(),
                        tuple: v.tuple.iter().map(|t| t.to_string()).collect(),
                    })
                    .collect(),
                *open,
                None,
                None,
            ),
            TypeKind::Opaque { repr } => (
                "opaque".to_string(),
                Vec::new(),
                Vec::new(),
                false,
                None,
                Some(repr.to_string()),
            ),
            TypeKind::Alias { target } => (
                "alias".to_string(),
                Vec::new(),
                Vec::new(),
                false,
                Some(target.to_string()),
                None,
            ),
        };
        types.push(TliType {
            kind,
            name: info.name.clone(),
            generics: info.generics.clone(),
            fields,
            variants,
            open,
            alias,
            repr,
        });
        if !info.docs.is_empty() {
            docs.push((format!("type:{}", info.name), info.docs.join("\n")));
        }
    }

    let mut interfaces = Vec::new();
    for info in env.interfaces.values() {
        if info.package != env.package || !info.public {
            continue;
        }
        interfaces.push(TliInterface {
            name: info.name.clone(),
            generics: info.generics.iter().map(|g| g.name.clone()).collect(),
            assoc: info.assoc_types.clone(),
            methods: info.methods.values().map(method_to_tli).collect(),
        });
        if !info.docs.is_empty() {
            docs.push((format!("interface:{}", info.name), info.docs.join("\n")));
        }
    }

    let mut impls = Vec::new();
    for imp in &env.impls {
        if imp.package != env.package {
            continue;
        }
        impls.push(TliImpl {
            self_ty: imp.self_ty.to_string(),
            iface: imp.iface.clone(),
            iface_args: imp.iface_args.iter().map(|t| t.to_string()).collect(),
            assoc: imp
                .assoc
                .iter()
                .map(|(k, v)| (k.clone(), v.to_string()))
                .collect(),
            generics: imp.generics.iter().map(|g| g.name.clone()).collect(),
            methods: imp.methods.values().map(method_to_tli).collect(),
        });
    }

    let mut functions = Vec::new();
    for info in env.functions.values() {
        if info.package != env.package || !info.public {
            continue;
        }
        functions.push(TliFunction {
            name: info.sig.name.clone(),
            generics: info.sig.generics.iter().map(|g| g.name.clone()).collect(),
            params: info.sig.params.iter().map(|t| t.to_string()).collect(),
            param_names: info.sig.param_names.clone(),
            ret: info.sig.ret.to_string(),
        });
        if !info.docs.is_empty() {
            docs.push((format!("fn:{}", info.sig.name), info.docs.join("\n")));
        }
    }
    // Deterministic and free of duplicates.
    functions.sort_by(|a, b| a.name.cmp(&b.name));
    let mut seen = std::collections::BTreeSet::new();
    functions.retain(|f| seen.insert(f.name.clone()));

    let consts = env
        .consts
        .iter()
        .filter(|(name, _)| {
            env.const_packages
                .get(*name)
                .map(|p| p == &env.package)
                .unwrap_or(env.package != "std.prelude")
        })
        .map(|(name, ty)| (name.clone(), ty.to_string()))
        .collect();

    let mut macros = Vec::new();
    for def in env.macros.values() {
        let digest = hash::hash_str(&macro_digest(def));
        macros.push(TliMacro {
            name: def.name.clone(),
            rules: def
                .rules
                .iter()
                .map(|rule| {
                    (
                        render_tokens(&rule.matcher),
                        render_tokens(&rule.transcriber),
                    )
                })
                .collect(),
            digest,
        });
    }
    macros.sort_by(|a, b| a.name.cmp(&b.name));
    docs.sort();

    let mut tli = Tli {
        format_version: FORMAT_VERSION,
        abi: COMPILER_ABI.to_string(),
        compiler_version: COMPILER_VERSION.to_string(),
        package: env.package.clone(),
        content_hash: [0; 32],
        fingerprint,
        builtin: false,
        deps,
        types,
        interfaces,
        impls,
        functions,
        consts,
        macros,
        instances: Vec::new(),
        docs,
    };
    tli.content_hash = tli.compute_content_hash();
    tli
}

fn method_to_tli(method: &crate::checker::MethodInfo) -> TliMethod {
    TliMethod {
        name: method.sig.name.clone(),
        generics: method.sig.generics.iter().map(|g| g.name.clone()).collect(),
        param_names: method.sig.param_names.clone(),
        params: method.sig.params.iter().map(|t| t.to_string()).collect(),
        ret: method.sig.ret.to_string(),
        has_body: method.body.is_some() || method.from_interface.is_some(),
    }
}

/// Compute a package fingerprint as described in DESIGN.md §4.1.
pub fn fingerprint(source_hash: Digest, deps: &[(String, Digest)]) -> Digest {
    let mut data = Vec::new();
    data.extend_from_slice(COMPILER_VERSION.as_bytes());
    data.extend_from_slice(COMPILER_ABI.as_bytes());
    data.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    data.extend_from_slice(std::env::consts::ARCH.as_bytes());
    data.extend_from_slice(std::env::consts::OS.as_bytes());
    data.extend_from_slice(&source_hash);
    for (name, digest) in deps {
        data.extend_from_slice(name.as_bytes());
        data.extend_from_slice(digest);
    }
    hash::sha256(&data)
}

/// Recompute the dependency list stored in a summary, for incremental checks.
pub fn dep_hashes(tli: &Tli) -> BTreeMap<String, Digest> {
    tli.deps.iter().cloned().collect()
}

/// Convenience used by the loader: check modules and emit a summary in one go.
pub fn check_and_emit(
    package: &str,
    modules: &[crate::ast::Module],
    deps: &[Tli],
    deps_hashes: Vec<(String, Digest)>,
    source_hash: Digest,
) -> (crate::checker::CheckResult, Tli) {
    let result = check_package(package, modules, deps);
    let fp = fingerprint(source_hash, &deps_hashes);
    let tli = emit(&result.env, deps_hashes, fp);
    (result, tli)
}

// ----------------------------------------------------------------------- codec

struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }

    fn raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    fn u32(&mut self, value: u32) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    fn bool(&mut self, value: bool) {
        self.buf.push(value as u8);
    }

    fn str(&mut self, value: &str) {
        self.u32(value.len() as u32);
        self.raw(value.as_bytes());
    }

    fn digest(&mut self, value: &Digest) {
        self.raw(value);
    }

    fn opt<T>(&mut self, value: &Option<T>, write: impl FnOnce(&mut Writer, &T)) {
        match value {
            Some(value) => {
                self.bool(true);
                write(self, value);
            }
            None => self.bool(false),
        }
    }

    fn list<T>(&mut self, values: &[T], mut write: impl FnMut(&mut Writer, &T)) {
        self.u32(values.len() as u32);
        for value in values {
            write(self, value);
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        if self.pos + count > self.bytes.len() {
            return Err("unexpected end of interface file".into());
        }
        let slice = &self.bytes[self.pos..self.pos + count];
        self.pos += count;
        Ok(slice)
    }

    fn u32(&mut self) -> Result<u32, String> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn bool(&mut self) -> Result<bool, String> {
        Ok(self.take(1)?[0] != 0)
    }

    fn str(&mut self) -> Result<String, String> {
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| "invalid UTF-8 in interface file".into())
    }

    fn digest(&mut self) -> Result<Digest, String> {
        let bytes = self.take(32)?;
        let mut digest = [0u8; 32];
        digest.copy_from_slice(bytes);
        Ok(digest)
    }

    fn opt<T>(
        &mut self,
        read: impl FnOnce(&mut Reader<'a>) -> Result<T, String>,
    ) -> Result<Option<T>, String> {
        if self.bool()? {
            Ok(Some(read(self)?))
        } else {
            Ok(None)
        }
    }

    fn list<T>(
        &mut self,
        mut read: impl FnMut(&mut Reader<'a>) -> Result<T, String>,
    ) -> Result<Vec<T>, String> {
        let count = self.u32()? as usize;
        // Guard against absurd allocations from corrupt input.
        if count > self.bytes.len() {
            return Err("invalid list length in interface file".into());
        }
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(read(self)?);
        }
        Ok(out)
    }

    fn at_end(&self) -> bool {
        self.pos == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_module;
    use std::collections::BTreeMap;

    #[test]
    fn summary_round_trips() {
        let source = r#"
package demo
pub struct Point { x: i32, y: i32 }
pub enum Shape { Circle { radius: f64 }, Empty }
pub fn classify(s: Shape) -> string { return "x"; }
"#;
        let module = parse_module(source, 0, "demo", &BTreeMap::new()).unwrap();
        let result = check_package("demo", &[module], &[]);
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let tli = emit(&result.env, Vec::new(), [0u8; 32]);
        let bytes = tli.encode();
        let decoded = Tli::decode(&bytes).unwrap();
        assert_eq!(decoded.package, "demo");
        assert_eq!(
            decoded
                .types
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Point", "Shape"]
        );
        assert_eq!(tli.content_hash, decoded.content_hash);
    }
}
