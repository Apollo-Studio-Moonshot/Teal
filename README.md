# Teal

Teal is a fast-compiling, extensible static language. This repository implements
the compiler front end described in [`DESIGN.md`](DESIGN.md): a hand-written
LL(1) lexer and parser, hygienic declarative macros, a per-function
bidirectional type checker, deterministic `.tli` interface summaries, and an
incremental, package-parallel build.

There is deliberately **no code generator yet**. The build pipeline is
"check and emit summaries", which is the prototype slice suggested in
DESIGN.md §7.

```
$ tlc check examples
  demo                            1 file(s)    121 lines   13 fn  checked   1fee7d8f0358  1ms
1 package(s), 121 line(s), 13 function(s), 0 cached, 3ms total
```

## Quick start

```sh
cargo build --release
cargo run -- check examples          # parse + type check, no artifacts
cargo run -- build examples          # write target/teal/demo.tli
cargo run -- build examples/workspace # multi-package DAG
cargo run -- deps examples/workspace  # print the package graph
cargo run -- clean examples
cargo test
```

The binary is named `tlc` (`cargo run` invokes it as well).

## Language

Teal is defined by the design document. A short tour of what this front end
accepts and verifies:

```teal
package demo

/// A struct is a product type. Fields are always explicit.
pub struct Point {
    x: f64,
    y: f64,
}

/// Derives are declarative macros over the item's token tree.
#[derive(Eq, Hash, Show)]
pub struct Id { value: i64 }

/// Enums are closed by default, so exhaustiveness is decidable locally.
pub enum Shape {
    Circle { radius: f64 },
    Rect { w: f64, h: f64 },
    Empty,
}

pub fn classify(s: Shape) -> string {
    return match s {
        Shape::Circle { radius } if radius > 100.0 => "big circle",
        Shape::Circle { .. } => "circle",
        Shape::Rect { w, h } if w == h => "square",
        Shape::Rect { .. } => "rect",
        Shape::Empty => "empty",
    };
}
```

Also implemented:

* Nominal interfaces, associated types, provided methods, generic impls, and
  `dyn` dispatch.
* Monomorphic generic checking with definition-site bounds (`T: Show`) and
  explicit turbofish (`f::<i32>(...)`) when inference is not local.
* `Result`/`Option` with `?`, including statically resolved `From` conversion.
* `Option`/`Result` match patterns, `if let`, `while let`, `let ... else`,
  or-patterns, range patterns, guards, and binding-with-destructure (`x @ pat`).
* Declarative macros with fragment specifiers, repetitions, separators,
  hygiene marks, recursion/size budgets, and exported rule tables.
* Newline-terminated statements (Go-style) with block-tail expressions.
* `defer`, `for`, `loop`, struct literals, cast expressions, and closures.

See `examples/hello.tl` and `examples/workspace/` for complete programs.

## CLI

```
tlc check [path] [--json]         parse + type check (no writes)
tlc build [path] [--target dir]   check and emit `.tli` summaries
tlc emit-tli [path]               alias for build
tlc deps [path]                   print the package dependency graph
tlc inspect <file.tli>            dump a summary
tlc clean [path]                  remove the artifact directory
```

Options: `--target <dir>`, `--jobs <n>`, `--json`, `-h/--help`.

Diagnostics are rendered with source snippets and stable error codes:

```
error[E0501]: non-exhaustive `match` on `Shape`; missing `Empty`
  --> examples/hello.tl:38:12
   |
38 |     return match s {
   |            ^^^^^^
   = note: add the missing patterns or a `_ =>` arm
```

## Packages and the incremental build

A package is a directory of `.tl` files. An optional `teal.pkg` manifest names
it:

```
name = acme.math
```

Imports (`import acme.math`) determine the package DAG. Packages whose
dependencies are ready compile concurrently with `std::thread::scope`; `--jobs`
bounds the parallelism. Output is independent of scheduling order.

For each package the loader computes

```
fp = H(compiler_version, ABI, format_version, target_arch/os,
       H(sorted source files), deps' .tli content hashes)
```

and stores it beside the summary (`target/teal/<package>.tli`, `.fp`, `.tld`).
If it matches and the summary decodes, the package is a no-op. Because a
dependency's content hash only covers its **exported interface** (not private
bodies), body-only changes in a dependency do not invalidate its dependents.
The test suite exercises both cases.

Summaries are never rewritten when their bytes are unchanged, so a no-op build
touches no artifacts.

## Architecture

| Module | Responsibility |
| --- | --- |
| `src/lexer.rs` | LL(1) lexer, newline statement termination, doc comments |
| `src/ast.rs` | Surface AST; the only IR in the prototype |
| `src/parser.rs` | Recursive-descent parser, inline macro expansion, derives |
| `src/macros.rs` | Token trees, rule matching, transcription, hygiene |
| `src/types.rs` | Semantic types, canonical rendering/parsing |
| `src/checker.rs` | Environment construction, per-body checker, exhaustiveness |
| `src/tli.rs` | Deterministic binary interface summaries and fingerprints |
| `src/loader.rs` | Discovery, DAG scheduling, incremental cache, reports |
| `src/hash.rs` | Dependency-free SHA-256 for Merkle fingerprints |
| `src/source.rs` | Source map and diagnostic rendering |
| `src/main.rs` | `tlc` CLI |

### Checking model

Each function body (and each interface-provided method body, and each impl
method body) is checked independently.

* Signatures are fully elaborated before bodies are checked.
* `synth(expr)` infers; `check(expr, expected)` propagates expectations, which
  is what makes integer literals, enum constructors, and closures work without
  whole-function inference.
* No unification state escapes a body. Call-site type-argument inference is
  local to one call expression: explicit turbofish first, then the expected
  return type, then argument types.
* Impl selection is an O(impls) head lookup. Bounds are checked at definition
  sites; call sites verify that concrete type arguments implement the declared
  bounds.
* Match exhaustiveness uses a constructor matrix over closed enums, bools,
  structs and tuples; `open enum` requires a wildcard arm.
* The orphan rule is enforced: an impl must live in the package defining the
  type or the interface.

### Interface summaries (`.tli`)

The binary format is versioned by `FORMAT_VERSION` and tagged with a compiler
ABI. It contains package identity, content hash, dependency hashes, and
deterministically sorted sections for types, interfaces, impls, functions,
constants, macros, instances and docs. Every digest is SHA-256 over a canonical
serialization. `tlc inspect` prints a decoded summary.

Dependency macros are serialized as rule token sources and reconstructed when
a dependent package parses, so downstream code can invoke a dependency's macro
without reading its source.

### Determinism

* Symbol and summary ordering is sorted, never traversal- or thread-dependent.
* Type argument canonicalization is structural (`Type`'s `Display`).
* Diagnostics are sorted and deduplicated before rendering.
* Artifact writes are content-compared and skipped when identical.

## Tests

`cargo test` runs:

* Lexer newline/lexing units.
* SHA-256 known vectors and type-string round trips.
* Macro matching, repetition, and hygiene units.
* Parser coverage for the design's surface syntax.
* Checker behavior: exhaustive/non-exhaustive matches, type mismatches,
  immutable assignment, `?` conversion, bounds, derives, provided methods,
  `let ... else`, range patterns, open enums, orphan impls.
* `.tli` round-trip determinism.
* Incremental builds, multi-package DAGs, dependency macro export, unknown
  imports.

## Current limitations (front-end prototype)

These are deliberate scope boundaries, not accidents:

* **No codegen or runtime.** `tlc build` emits summaries only.
* **No borrow checker.** `&T`/`&mut T` are checked structurally; escaping
  borrows are not yet rejected.
* **Generic bodies are not serialized** in `.tli`; only signatures and macros
  cross package boundaries. Downstream packages can call generic functions but
  there is no monomorphization/codegen to instantiate.
* **Prelude is intrinsic.** `Option`, `Result`, `Vec`, slices, `Eq`, `Hash`,
  `Show`, `From`, `Into`, `Iterator` and the `println!` family are built into
  the checker as trusted declarations rather than parsed from `.tl` sources.
* **Macro definitions are file-scoped during parsing** and dependency-scoped
  through summaries; a macro defined in a later file of the same package is not
  visible earlier in that package.
* **Name resolution is flat within the visible package graph.** Explicit
  imports are parsed and used to build the package DAG, but cross-package names
  resolve by their final segment rather than enforcing per-file import lists.
* **Interface supertraits are parsed but not consulted during lookup.** Inline
  generic bounds, associated types, provided methods and generic impls are fully
  checked; a supertrait's methods are not yet inherited.
* **Generic type aliases, GATs, `impl Trait`, and operator overloading are
  rejected or absent**, per the design.
* **No const evaluation, async, or procedural macros**, per the design.

## Next steps

The natural continuation is the one DESIGN.md recommends after the front end:
typed-IR lowering, Cranelift codegen per function, instance caching with
content-addressed objects, and weak/COMDAT merging at link time. The current
checker already produces the per-function, fully annotated environment that
lowering needs.
