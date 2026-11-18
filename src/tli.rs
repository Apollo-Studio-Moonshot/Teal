use crate::digest;
use crate::parser::{ItemKind, Module};
use std::collections::BTreeMap;
use std::fmt::Write as _;

pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tli {
    pub magic: String,
    pub format_version: u32,
    pub package: String,
    pub content_hash: [u8; 32],
    pub symbols: BTreeMap<String, String>,
    pub types: BTreeMap<String, Vec<String>>,
    pub interfaces: BTreeMap<String, Vec<String>>,
    pub impls: BTreeMap<String, Vec<String>>,
}

pub fn emit(module: &Module, source: &str) -> Tli {
    let mut symbols = BTreeMap::new();
    let mut types = BTreeMap::new();
    let mut interfaces = BTreeMap::new();
    let mut impls = BTreeMap::new();

    for item in &module.items {
        match &item.kind {
            ItemKind::Const { name, .. } => {
                symbols.insert(name.clone(), "const".into());
            }
            ItemKind::Struct { name, fields, .. } => {
                symbols.insert(name.clone(), "struct".into());
                types.insert(
                    name.clone(),
                    fields
                        .iter()
                        .map(|f| format!("{}:{}", f.name, crate::checker::type_name(&f.ty)))
                        .collect(),
                );
            }
            ItemKind::Enum {
                name,
                variants,
                open,
                ..
            } => {
                symbols.insert(name.clone(), "enum".into());
                let mut entries = Vec::new();
                entries.push(if *open {
                    "open".into()
                } else {
                    "closed".into()
                });
                entries.extend(variants.iter().map(|v| v.name.clone()));
                types.insert(name.clone(), entries);
            }
            ItemKind::Opaque { name, ty } => {
                symbols.insert(name.clone(), "opaque".into());
                types.insert(name.clone(), vec![crate::checker::type_name(ty)]);
            }
            ItemKind::TypeAlias { name, ty } => {
                symbols.insert(name.clone(), "type".into());
                types.insert(name.clone(), vec![crate::checker::type_name(ty)]);
            }
            ItemKind::Interface { name, methods } => {
                symbols.insert(name.clone(), "interface".into());
                interfaces.insert(
                    name.clone(),
                    methods.iter().map(|m| m.name.clone()).collect(),
                );
            }
            ItemKind::Impl {
                ty, iface, items, ..
            } => {
                let key = format!(
                    "{} for {}",
                    iface.as_deref().unwrap_or("inherent"),
                    crate::checker::type_name(ty)
                );
                impls.insert(key, items.iter().map(|m| m.name.clone()).collect());
            }
            ItemKind::Fn(function) => {
                symbols.insert(
                    function.name.clone(),
                    format!("fn({})", function.params.len()),
                );
            }
            ItemKind::Macro { name, .. } => {
                symbols.insert(name.clone(), "macro".into());
            }
        }
    }

    Tli {
        magic: "TLI\0".into(),
        format_version: FORMAT_VERSION,
        package: module.package.clone().unwrap_or_else(|| "default".into()),
        content_hash: digest(source.as_bytes()),
        symbols,
        types,
        interfaces,
        impls,
    }
}

pub fn encode(tli: &Tli) -> Result<Vec<u8>, String> {
    let mut text = String::new();
    writeln!(text, "version={}", tli.format_version).unwrap();
    writeln!(text, "package={}", tli.package).unwrap();
    writeln!(
        text,
        "hash={}",
        tli.content_hash
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
            .replace('\n', "")
    )
    .unwrap();
    writeln!(text, "[symbols]").unwrap();
    for (name, kind) in &tli.symbols {
        writeln!(text, "{}={}", name, kind).unwrap();
    }
    writeln!(text, "[types]").unwrap();
    for (name, fields) in &tli.types {
        writeln!(text, "{}={}", name, fields.join("|")).unwrap();
    }
    writeln!(text, "[interfaces]").unwrap();
    for (name, methods) in &tli.interfaces {
        writeln!(text, "{}={}", name, methods.join("|")).unwrap();
    }
    writeln!(text, "[impls]").unwrap();
    for (name, methods) in &tli.impls {
        writeln!(text, "{}={}", name, methods.join("|")).unwrap();
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"TLI\0");
    out.extend_from_slice(text.as_bytes());
    Ok(out)
}

pub fn decode(bytes: &[u8]) -> Result<Tli, String> {
    if bytes.len() < 4 || &bytes[..4] != b"TLI\0" {
        return Err("invalid TLI magic".into());
    }
    let text = String::from_utf8(bytes[4..].to_vec()).map_err(|e| e.to_string())?;
    let mut tli = Tli {
        magic: "TLI\0".into(),
        format_version: FORMAT_VERSION,
        package: "default".into(),
        content_hash: [0u8; 32],
        symbols: BTreeMap::new(),
        types: BTreeMap::new(),
        interfaces: BTreeMap::new(),
        impls: BTreeMap::new(),
    };
    let mut section = "";
    for line in text.lines() {
        if line.starts_with("package=") {
            tli.package = line[8..].into();
        } else if line.starts_with("version=") {
            tli.format_version = line[8..].parse().unwrap_or(FORMAT_VERSION);
        } else if let Some(hex) = line.strip_prefix("hash=") {
            let mut hash = [0u8; 32];
            for (i, pair) in hex.as_bytes().chunks(2).enumerate() {
                if i >= 32 {
                    break;
                }
                if pair.len() == 2 {
                    hash[i] = u8::from_str_radix(&String::from_utf8_lossy(pair), 16).unwrap_or(0);
                }
            }
            tli.content_hash = hash;
        } else if line == "[symbols]" {
            section = "symbols";
        } else if line == "[types]" {
            section = "types";
        } else if line == "[interfaces]" {
            section = "interfaces";
        } else if line == "[impls]" {
            section = "impls";
        } else if let Some((key, value)) = line.split_once('=') {
            let values = if value.is_empty() {
                Vec::new()
            } else {
                value.split('|').map(str::to_string).collect()
            };
            match section {
                "symbols" => {
                    tli.symbols.insert(key.into(), value.into());
                }
                "types" => {
                    tli.types.insert(key.into(), values);
                }
                "interfaces" => {
                    tli.interfaces.insert(key.into(), values);
                }
                "impls" => {
                    tli.impls.insert(key.into(), values);
                }
                _ => {}
            }
        }
    }
    Ok(tli)
}

pub fn fingerprint(source_hash: [u8; 32], dep_hashes: impl Iterator<Item = [u8; 32]>) -> [u8; 32] {
    let mut data = b"teal-fingerprint-v1".to_vec();
    data.extend_from_slice(&source_hash);
    for dep in dep_hashes {
        data.extend_from_slice(&dep);
    }
    digest(data)
}
