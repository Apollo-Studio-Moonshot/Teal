# Teal — a fast-compiling, extensible static language

Status: design draft (v0). Nothing here is implemented yet.

Teal's thesis: **almost all compile-time cost in a modern language comes from four
things — whole-program inference, procedural macros, trait solving, and the passes you
need because the first three destroyed locality.** Remove them and you can have ADTs,
pattern matching, interfaces with associated types, monomorphized generics, hygienic
macros and `Result` propagation while still compiling at roughly Go speed.

The target is a full rebuild of a 1M-line codebase in single-digit seconds on 8 cores,
and an incremental build in under 200 ms regardless of codebase size.

---

## 1. North star

| Property | Decision |
| --- | --- |
| Type checking | Bidirectional, **per function**, no unification variables escaping a function body |
| Top-level inference | **None.** Every public signature is fully annotated and fully explicit |
| Generics | Monomorphic (specialized), definition-site checked, instance-cached |
| Interfaces | Nominal, explicit `impl`, associated types, no GATs |
| Macros | Declarative, hygienic, pure token rewriting, no type information |
| Errors | `Result` + `?` with statically resolved conversion |
| Backend | Cranelift (debug) / LLVM (release), plus link-time COMDAT dedup |
| Parsing | Hand-written LL(1) recursive descent, single token of lookahead, single AST |
| Concurrency | Package-level DAG parallelism + intra-package function parallelism |

Everything below is a consequence of that table.

---

## 2. What Teal deliberately refuses

Each of these is a *deliberate* omission with a stated reason. This list is the most
important part of the design; it is what buys the compile speed.

| Refused | Why |
| --- | --- |
| Hindley–Milner / inference across function boundaries | Requires whole-program dependency tracking, breaks per-function parallelism and per-function incrementality |
| Procedural macros / compiler plugins | Arbitrary code execution at parse time; forces full AST serialization, breaks caching, ruins determinism and parallel expansion |
| Blanket + overlapping impls with negative reasoning | Trait solving becomes a search (prolog-ish), needs global coherence, kills O(1) impl lookup |
| `impl Trait` in argument/return position | Leaks hidden types into interface summaries; forces body-driven signature computation |
| Higher-kinded types, GATs | Need use-site bound re-checking and richer summaries; large complexity for small payoff |
| Operator overloading, user-defined operators | Scanner stays context-free; no deparser, no precedence tables, no parse ambiguity |
| Lifetime/borrow checker (`&'a T`) | Adds a second whole-function analysis pass and a signature-level effect system |
| Exceptions / unwinding | `Result` is the only error channel; `defer` needs no landing pads |
| Indentation-sensitive syntax | Keeps lexer a 40-line-per-construct state machine |
| Structural subtyping | Nominal interfaces make conformance an O(1) table lookup instead of a fixpoint |

If a feature cannot be checked by looking at *one function plus a hash-indexed summary
table*, it does not go in Teal.

---

## 3. Surface syntax draft

File extension `.tl`. One file belongs to one package; a package is a directory.

### 3.1 Packages and imports

```teal
package http.cache

// explicit imports only; no glob imports at top level
import std.io
import std.strings as str
import std.collections.{HashMap, MapIter}

// items are `pub` or private (default). There is no `pub(crate)`.
pub const MAX_ENTRIES: usize = 1 << 16
```

Glob imports (`import foo.*`) are banned: they make name resolution results depend on
the transitive import graph and defeat per-file resolution. A single explicit
`import foo` gives you `foo::Bar`; a braced list gives you bare names.

### 3.2 ADTs

```teal
struct Point {
    x: f64,
    y: f64,
}

enum Shape {
    Circle { radius: f64 },
    Rect   { w: f64, h: f64 },
    Empty,
}

// newtype with a real identity, fields visible only through impls
opaque Handle = i64

// plain alias, always transparent
type Idents = [string]
```

* Structs are **product types only**. No inheritance, no vtables, no field defaults.
* Enums are **closed** by default: downstream packages may not add variants, so
  exhaustiveness is decidable from the defining package's summary alone. `open enum`
  is an opt-in escape hatch that forces a `_ =>` arm downstream.
* Every field requires an explicit type. No field inference.

### 3.3 Pattern matching

`match` is an expression. Exhaustiveness is checked with a decision-tree compilation
(Maranget), which is linear in pattern count and produces jump tables rather than a
chain of tests.

```teal
pub fn classify(s: Shape) -> string {
    return match s {
        Shape::Circle { radius } if radius > 100.0 => "big circle",
        Shape::Circle { .. }                       => "circle",
        Shape::Rect { w, h } if w == h             => "square",
        Shape::Rect { .. }                         => "rect",
        Shape::Empty                               => "empty",
    };
}
```

Supported pattern forms:

```
pat := "_"
     | literal                     // 0, 1.5, "s", 'c', true
     | ident                       // irrefutable binding
     | path ( "{" field_pat_list "}" )?    // ADT destructuring
     | pat "|" pat                 // or-pattern, bindings must agree
     | lo "..=" hi                 // inclusive integer/char/float range
     | "&" "mut"? pat              // borrow pattern
     | ident "@" pat               // binding with destructure
field_pat := field ":" pat | field | ".."
```

Also `if let`, `while let` and `let else`:

```teal
let Some(cfg) = load_config(path) else { return Err(MissingConfig { path }) };
```

### 3.4 Interfaces and associated types

Interfaces are nominal. Implementations must be declared in the package that defines
either the type or the interface (orphan rule), which makes coherence a *per-package*
check — no global scan, no overlap search, no negative reasoning.

```teal
interface Iterator {
    type Item

    fn next(self: &mut Self) -> Option<Self::Item>

    // provided method: compiled once per implementing type, lazily
    fn count(self: &mut Self) -> usize {
        let mut n = 0;
        while self.next().is_some() { n += 1; }
        return n;
    }
}

interface Into<T> {
    fn into(self) -> T
}

impl Iterator for Range {
    type Item = i64

    fn next(self: &mut Self) -> Option<i64> {
        if self.cur >= self.end { return None; }
        let v = self.cur;
        self.cur += 1;
        return Some(v);
    }
}
```

Rules that keep checking local:

* Bounds are checked at the **definition site** ("definition-site bound checking"). The
  body of `Iterator::count` is type-checked exactly once, against the declared bound,
  never per instantiation.
* Implementation selection at a call site is a hash lookup, never a search:
  1. receiver is a type parameter → use its bound (`Self::Item` resolves through the
     bound's associated-type equation);
  2. receiver is a concrete type `C` → look up `impl I for C` in the impl table of the
     package that defines `C`, then of the package that defines `I`;
  3. otherwise: error, with the two candidate packages named.
* Generic impls are allowed (`impl<T: Show> Show for [T]`) because the head
  constructor disambiguates the lookup; they never overlap by construction.
* Dyn dispatch is opt-in and explicit: `dyn Iterator<Item = i64>`. There is no
  `impl Trait`.

### 3.5 Generics

```teal
struct Cache<K: Eq + Hash, V> {
    entries: HashMap<K, V>,
    capacity: usize,
}

impl<K: Eq + Hash, V> Cache<K, V> {
    pub fn get_or_insert_with<F: Fn() -> V>(self: &mut Self, key: K, f: F) -> &V {
        if self.entries.contains(&key) {
            return self.entries.get(&key);
        }
        self.entries.insert(key, f());
        return self.entries.get(&key);
    }
}
```

* Bounds are inline (`K: Eq + Hash`). A `where` clause exists only as sugar for
  long lists; it is desugared during parsing and never re-consulted later.
* Type arguments in expression position use the turbofish: `Vec::<i32>::new()`. This
  is what removes the `a < b` vs `a::<b>` ambiguity and lets the parser stay LL(1).
* No generic type aliases, no associated type defaults, no GATs.
* Generic *items* are type checked **once, generically**. Monomorphization happens in
  lowering/codegen only, by substitution into a typed IR template.

### 3.6 Macros (declarative, hygienic)

Macros operate on token trees, never on types, never at name-resolution time. They are
expandable from the interface summary alone, so a downstream package can expand a
macro without reading the defining package's source.

```teal
macro vec! {
    () => { Vec::new() },

    ($($x:expr),+ $(,)?) => {{
        let mut v = Vec::new();
        $( v.push($x); )+
        v
    }},
}

macro log_ctx! {
    ($fmt:literal $(, $arg:expr)*) => {{
        let __ctx = current_context();          // hygienic: cannot capture caller's
        println!("[{}] " + $fmt, __ctx.name $(, $arg)*);
    }},
}
```

* Fragment specifiers: `$x:ident`, `:literal`, `:tt`, `:expr`, `:pat`, `:ty`, `:path`,
  `:block`, `:stmt`, `:item`, `:vis`.
* Repetitions: `$( ... ),*` / `;*` / `*` (no separator), with `?` and `+`.
* Hygiene by **lexical marks**: every identifier introduced by a template is renamed to
  `sym#m` where `m = hash(def_id, expansion_stack)`. Binding forms, labels and
  `self` are hygienic; *item* names (fn/struct/impl names) are call-site visible.
  `$crate` / `$self` expands to the defining package's path.
* Expansion is bounded: recursion depth 128, total expansion size 1 MiB per invocation.
  Exceeding either is a hard error, not a silent degrade.
* Macro invocations are expanded **within the file that invokes them**. A macro cannot
  create items used by another file in the same package.

**Derives are macros over token-level field lists**, not a built-in closed set:

```teal
#[derive(Eq, Hash, Show)]
struct Point { x: i32, y: i32 }

// user-defined: the compiler feeds the struct's field tokens into your macro
macro derive_hash! {
    (struct $n:ident { $($f:ident : $t:ty),* $(,)? }) => { ... },
}
```

`#[derive(Eq, Hash, ...)]` looks up a macro named `derive_<lowercase>` in scope and
invokes it with the item's token tree. This recovers most of the value of procedural
derives with zero extra compiler machinery, and the expansion is still pure, cached and
hygienic.

### 3.7 Errors: `Result` and `?`

`Result<T, E>` and `Option<T>` live in the prelude. There are no exceptions.

```teal
enum CacheError {
    Io(io::Error),
    Full { capacity: usize },
}

impl From<io::Error> for CacheError {
    fn from(e: io::Error) -> CacheError { return CacheError::Io(e) }
}

pub fn get_or_load<V, F: Fn() -> Result<V, io::Error>>(
    self: &mut Self, key: string, load: F,
) -> Result<&V, CacheError> {
    if self.len >= MAX_ENTRIES {
        return Err(CacheError::Full { capacity: MAX_ENTRIES });
    }
    let v = load()?;          // E=io::Error converts to CacheError via `From`
    ...
}
```

`?` is a **purely local, syntactic+type-resolved** operator:

1. The enclosing function's return type is always annotated, and therefore the
   *carrier* (`Result<_, F>` or `Option<_>`) is known without inference.
2. `expr?` requires `expr: Result<T, E>` when the carrier is `Result`, or `Option<T>`
   when it is `Option`.
3. If `E == F`, no conversion code is emitted at all. Otherwise the compiler looks up
   `impl From<E> for F` by the same O(1) rule as any other interface call and records a
   direct call to it. If no impl exists, the error points at the `?`, not at the callee.
4. `try { ... }` takes its carrier from the enclosing function's return type, so it
   never introduces an inference variable.

Consequence: `Result` conversion costs one hash lookup per `?` site, and zero runtime
work in the common same-error-type case. Trade-off: no `Try`/`FromResidual` genericity —
you cannot `?` a custom monad.

### 3.8 Statements, control flow, memory (brief)

```teal
fn demo(xs: [i32]) -> i32 {
    let mut total = 0;
    for x in xs.iter() {
        if x < 0 { continue; }
        total += x;
    }
    defer log!("total = {}", total);     // runs at scope exit
    return total;
}
```

* Statement terminators are inserted at newlines using a Go-style rule (newline becomes
  `;` when the previous token is an identifier, literal, `)`, `]`, `}` or `?`). One
  consequence: you cannot break a line *before* `.method()` or a binary operator.
* `defer` is a scope-exit action; `panic` aborts by default, so no landing pads.
* Memory model (deliberately thin in v0): automatic reference counting for
  `@T` heap values, value semantics for structs, and **non-escaping borrows**: `&T` /
  `&mut T` are valid only for the duration of a call expression and may not be stored
  in a struct, captured by a closure that outlives the function, or returned. The
  escape check is a simple syntactic lattice evaluated per function body — it is
  function-local, so it costs nothing to the architecture. A real borrow checker is a
  post-1.0 project.

### 3.9 Grammar shape (why LL(1) works)

```ebnf
item        := attrs vis? item_kind
item_kind   := "package" path | "import" import | struct | enum | opaque | type
             | interface | impl | fn | macro | const
struct      := "struct" Ident generics? "{" (field ",")* "}"
field       := Ident ":" type
enum        := "enum" Ident generics? "{" (variant ",")* "}"
variant     := Ident ("{" (field ",")* "}" | "(" type ("," type)* ")")?
interface   := "interface" Ident (":" bound_list)? "{" iface_item* "}"
iface_item  := "type" Ident | fn_sig | fn_def
impl        := "impl" generics? path ("for" type)? "{" impl_item* "}"
fn          := "fn" Ident generics? "(" params? ")" ("->" type)? block

stmt        := let | var | assign_or_expr | if | while | for | loop | match
             | return | break | continue | defer | block
expr        := assign | binary | unary | postfix | atom
postfix     := atom ("(" args ")" | "." Ident (turbofish? "(" args ")")?
                     | "[" expr "]" | "?" | "as" type)*
atom        := literal | path turbofish? | "(" expr ")" | "[" elems "]" | "{" ... "}"
             | "match" expr "{" arm* "}" | "if" ... | closure | "try" block
```

Guarantees that make single-pass parsing viable:

* Every item begins with a keyword; every statement begins with a keyword, `{`, or a
  primary expression. One token of lookahead resolves all of it.
* No user-defined operators, no implicit conversions, no whitespace sensitivity beyond
  the newline rule, no nested block comments, no arbitrary raw-string delimiters.
* Type position never starts a statement, so `<` is unambiguous; expression-position
  type arguments require `::<`.
* Closures `|x: T| expr` are recognized at *expression* start with two tokens of
  lookahead (`|` then `|` or an identifier). Still linear, still no backtracking.

---

## 4. Compiler architecture

```mermaid
graph TD
    L[Loader: package DAG + fingerprints] --> P

    subgraph per-package task, run in parallel across the DAG
        P[Lex + parse to IR<br/>macro expansion inline] --> R[Resolve names<br/>from deps' .tli only]
        R --> T[Type check<br/>per function, parallel]
        T --> M[Lower to typed IR<br/>per function, parallel]
        M --> C[Codegen<br/>per function, parallel]
        T --> S[Emit .tli interface summary]
    end

    C --> O[.o objects + instance cache]
    S --> L
    O --> K[Link: dedupe COMDAT instances]
```

### 4.1 Pipeline detail

**Phase 0 — loader.** Build the package DAG from `teal.pkg` files. For each package
compute a fingerprint:

```
fp = H(compiler_version, target_triple, feature_flags,
       H(source files), H(deps' .tli content hashes))
```

If `fp` matches the recorded one and every emitted object exists, the package is a
no-op: it contributes its `.tli` and object list to the link step and nothing else.
This is the whole incremental build story at package granularity — cheap and blunt.

**Phase 1 — lex + parse + expand (one traversal).** Hand-written lexer emitting
tokens into a chunked arena (no per-token allocation, no interning locks: each file
uses a thread-local string arena merged at end of file). Parser is recursive descent
that writes **directly into the typed IR**, not into a general-purpose AST: type slots
are pre-allocated and left as `Hole` for the checker to fill. There is no
`AST → HIR → MIR` cascade; there is `IR` and then `IR` with holes filled.

Macro expansion is interleaved with parsing: when the parser sees `name!`, it matches
the token tree against the macro's rule table and re-enters the parser on the expanded
token stream, carrying hygiene marks. Expansion is memoized by
`H(macro_def_digest, argument_token_digest)` so repeated invocations (`vec!` in a hot
module) cost one expansion.

**Phase 2 — name resolution.** Resolved against dependencies' `.tli` files only; source
files of dependencies are never read. Because imports are explicit (no globs), each
file's resolution is independent and can run in parallel. There are exactly two
namespaces (types/modules, values) plus a macro namespace — no overloading by arity,
no operator names, no implicit conversions to disambiguate.

**Phase 3 — type check.** Unit of work is a *function body*, plus one unit per
generic definition (checked generically) and one unit per `impl` block.
Bidirectional checking: `check(expr, expected)` and `synth(expr) -> type`. Local
`let x = expr` synthesizes from `expr`; every other binding is annotated or immediately
consumed by a `match`. There is no unification state that survives the function, so a
function body can be checked in a work-stealing task with a `&`-shared package symbol
table and `&mut`-local scratch.

Conformance and impl lookup are memoized in a per-package table
`(interface_id, concrete_type_id) -> impl_ref`, which is also what the `.tli` exports.

**Phase 4 — lower + codegen.** Lowering substitutes type arguments into the typed IR
template for each *required* instantiation. Codegen runs per function with Cranelift
(fast, ~10x faster than LLVM at ~60–70% of optimized quality) or LLVM when
`--release-opt` is requested. Objects are written to a content-addressed path
`cache/obj/<fn_digest>.o`.

**Phase 5 — link.** Instances that were generated in more than one package are emitted
as **weak/COMDAT symbols**, so duplicate monomorphizations merge for free at link time
and the linker's dedup is the safety net for speculative instance generation.

### 4.2 The `.tli` interface summary

One flat, zero-copy (de)serializable binary file per package. It is the *only* thing a
downstream package ever reads about its dependencies.

```
magic "TLI\0" | format_version | compiler_abi
package_id | content_hash | deps: [(package_id, tli_hash)]
flags | target | feature_set_hash

section SYMBOLS     name -> sym_id, kind, type_digest, sig (flat type table)
section TYPES       ADT/enum/opaque layout, variant list, field types, closed|open,
                    where the impl table lives
section IFACES      method list, associated types, provided-method bodies (IR)
section IMPLS       (type_head, iface) -> assoc-type bindings + method refs
section MACROS      rule tables as token trees + hygiene metadata
section GEN_TMPL    typed IR bodies for public generics + their transitive
                    private-generic closure
section INSTANCES   inst_key -> symbol name, body digest  (things we already emitted)
section DOCS        (separate companion file, never needed for checking)
```

Every digest is a Merkle hash over a canonical serialization, so a signature change
propagates automatically to dependents' fingerprints while a body-only change does not.

### 4.3 Monomorphic generics with instance caching

The instance key is

```
inst_key = H(generic_def_digest, canonical_args(type_args),
             target_triple, opt_level, feature_set)
```

and canonicalization of type arguments makes structurally identical types produce
identical keys across packages (so `Vec<(i32, i32)>` built up in two different packages
collides, as desired).

Two-layer caching:

1. **Per-build instance cache** — `~/.cache/teal/inst/<arch>/<inst_key>.o`. Codegen
   consults it before specializing. Misses are written atomically and shared between
   concurrently building packages via `rename(2)`.
2. **Declared instances** — a library can ship hot specializations:
   `teal build --emit-instances 'Vec<u8>,Vec<String>,HashMap<string,Json>'`, which
   records them in `section INSTANCES`. Downstream packages see the instance in the
   dependency's summary and skip generating it.

Because specialization is duplicated-but-merged, **no global instance registry is
required during the build** — that is what keeps package compilation embarrassingly
parallel.

One honest catch: a downstream package may need to specialize a *private* generic used
inside a public generic body. `.tli` therefore serializes the transitive generic
closure of public generic bodies (usually tiny, since generic functions are a minority
of code). Private non-generic bodies are never exported.

### 4.4 Why per-function incrementality is cheap here

Each function records a **dependency fingerprint**: the sorted, deduped list of
`(symbol_id, signature_digest)` it touched, plus the body digests of anything it was
inlined into. A rebuild:

* body-only change → function's own digest changes, dependents' fingerprints do *not*
  → only that function re-checks and re-codegens;
* signature change → dependents' fingerprints change → exactly those functions
  recompile;
* interface summary format change → everything invalidates (bump `format_version`
  deliberately, rarely).

This works only because no function's type depends on another function's *body*.
That is the same property that forbids `impl Trait` and full inference — it is worth the
expressiveness loss.

### 4.5 Performance budget

Targets, per core, for non-optimizing builds (order-of-magnitude goals, to be validated
with a prototype before committing to the backend):

| Phase | Budget |
| --- | --- |
| Lex + parse + expand | 15–40 MB/s |
| Resolve (all deps cached, `.tli` mmap'd) | ~1 µs/file + O(symbols) |
| Type check | 100k–300k LOC/s |
| Lower + Cranelift codegen | 50k–150k LOC/s |
| Full cached rebuild | < 200 ms wall clock |
| Total passes over a function body | ≤ 4 (parse, check, lower, emit) |

Rough expectation: **2–4x faster than Go for full builds** and much faster for
incremental builds, but an order of magnitude slower than a purely streaming compiler
with no type checking. If we hit Go parity on full builds, the design succeeded.

### 4.6 Determinism

Parallel scheduling must never influence output. Concretely: symbol ids derive from
`H(package_hash, declaration_path)` not from traversal order; type interning tables are
per-thread and merged in sorted order; module-level tables are sorted before
serialization; `.tli` records a stable ordering. Reproducible builds are a test in CI,
not an aspiration.

---

## 5. Key trade-offs

### 5.1 Speed bought with expressiveness (the big four)

**No whole-program inference.** You write `let xs: Vec<i32> = Vec::new()` where Rust
would infer. Closure parameter types must be annotated when a closure is not
immediately constrained by a typed expected type. The payoff is enormous: per-function
checking, per-function incrementality, per-function parallelism, and error messages that
never depend on program size.

**No procedural macros.** You lose: custom `#[derive]` logic with real type
information, attribute macros that rewrite items, `include!`-style codegen, and
compile-time reflection (`sqlx::query!`-style). You keep: declarative derives over token
field lists, all of Rust's `macro_rules` power, and hygiene. If you genuinely need
procedural codegen, the sanctioned escape is *build-time code generation into `.tl`
files* — coarse, visible, cacheable, and it cannot affect parsing semantics of
hand-written code.

**No trait solving.** No blanket impl search chains, no specialization, no negative
bounds, no `dyn`-coercion inference. Coherence is per-package, impl selection is a hash
lookup. You pay with a less clever type system: `impl<T: Show> Show for Vec<T>` works,
but a bound like "any `T` whose `Item` is `Show`" resolves only through the declared
bounds on `T`, not through inference over the impl graph.

**No `impl Trait`.** Return-position `impl Iterator<Item=i32>` is a convenience whose
cost is hidden types in interfaces. Teal makes you name the type (or use `dyn`). This is
the single most common ergonomic complaint you should expect, and the answer is: name
your types, and the compiler will always be able to tell you why anything is fast.

### 5.2 Definition-site bound checking vs use-site

Checking a generic body once, generically, is what makes compile time independent of
instantiation depth. The cost is that the body may not use anything not implied by the
bounds (`T: Show` does not give you `T: Eq` even if every current instantiation has it),
so generic code sometimes needs redundant-looking bounds. Use-site checking would be
more permissive and much more expensive, and it destroys the `GEN_TMPL` sharing story.

### 5.3 Monomorphization vs compile-time bloat

Monomorphization gives you: no dictionary passing, no boxing, inlining across generic
boundaries, and predictable runtime performance. It costs: code size, and compile-time
blowup proportional to the number of *distinct* instantiations, which is the one lever
where a Teal user can still accidentally write a slow build. Mitigations: the instance
cache, `--emit-instances` for libraries, weak-symbol merging so duplication is not a
correctness problem, and a `--report-instances` diagnostic that ranks instantiations by
cumulative code size so the offending generic is obvious.

### 5.4 Interface summary cache: the coupling you accept

`.tli` is a contract. Its format version is a compiler ABI, and a format change
invalidates every cached artifact in the world. Design consequences:

* The summary must be *complete*: anything a downstream checker needs (including macro
  definitions and generic bodies) must live in it. That is why only public generics'
  bodies are serialized, and why macro definitions must be self-contained.
* Doc comments live in a companion file so that editing docs never re-checks anything.
* Zero-copy layout means the format is versioned explicitly and parsed with bounds
  checks, never with a generic deserializer.
* Macros in summaries are a real, if contained, hazard: the checker must expand
  downstream-provided token templates with the *dependency's* `$crate` mapping. Since
  expansion is pure and hygienic, this is safe, but it does mean a dependency bump can
  change the shape of an expanded macro without changing any source in your package —
  which is exactly why macro definition digests participate in the dependency
  fingerprint.

### 5.5 Pattern matching and exhaustiveness

Exhaustiveness needs complete variant lists, so closed-by-default enums are required
for the fast path. `open enum` pushes the burden to downstream `_ =>` arms and
serializes an "openness" flag into the summary. Adding a variant to a closed enum is a
breaking change that the interface fingerprint surfaces loudly — which is the correct
default but will annoy people who expect semver-compatible extension.

### 5.6 Hygiene and what macros cannot do

Lexical-mark hygiene means a macro cannot bind a name the caller sees, and cannot
capture the caller's variables. That is the desired behavior, but it also means
"macros that generate a variable for the caller to use" are impossible — you must
return a value instead. Combined with single-rule-set expansion confined to one file,
Teal macros are strictly less powerful than Rust's, and roughly as powerful as
`macro_rules` for the 90% case (constructors, test harnesses, builder boilerplate,
declarative derives).

### 5.7 No whole-program analysis

Without whole-program reachability you cannot: dead-strip at the AST level, devirtualize
`dyn` calls globally, or specialize based on call-site profiles. Mitigations: `dyn` is
opt-in and rare by design, `--gc-sections` handles unused code at link level, and
per-package `--strip-unused-impls` removes impls whose interface is unreferenced within
the package. Binary size will be larger than a whole-program compiler would produce;
compile time is the stated priority.

### 5.8 `?` without `Try`

Restricting `?` to `Result` and `Option`, keyed on the annotated return type, removes
the need for a `Try` interface with generic residual types. The cost is that you cannot
build async or effectful DSLs on top of `?`. If async is ever added, it must be added as
a first-class language feature whose state machine is generated during lowering, not as
library sugar over `?` — otherwise the invariant "carrier is known from the signature"
breaks.

### 5.9 Newline-terminated statements

Cheap lexer rule, clean code, and it aligns with the "no exceptions, no hidden
continuations" ethos. The cost is real: fluent chains must be formatted with the dot at
the end of the previous line or wrapped in parens, and generated code must be
newline-careful. This is the trade Teal makes with Go, and users will either not notice
or hate it forever.

### 5.10 What is *not* given up

ADTs with destructuring, exhaustive matching, associated types, generic impls, provided
methods, dyn dispatch when asked, hygienic macros, declarative derives, `defer`, and
zero-cost `Result` propagation with same-type errors. That is a genuinely modern
language; the omissions are all in the "type system cleverness" direction, not the
"everyday expressiveness" direction.

---

## 6. Open questions for v1

1. **Memory model.** ARC + non-escaping borrows is a placeholder. A full borrow checker
   is function-local (so it does not threaten the architecture) but adds a second pass
   over every body. Worth prototyping to measure.
2. **Async.** If added, it must be a lowering-level feature, never library sugar. Decide
   before `.tli` v1 or the format will churn.
3. **`const fn` evaluation.** Compile-time execution is a slow path that must be
   sandboxed and cached separately; interleaving it with type checking is the fastest
   way to lose the speed budget.
4. **Build-time codegen policy.** If declarative derives are insufficient in practice,
   the escape hatch is a `build.tl` producing `.tl` sources — not proc macros.
5. **Backend choice.** Cranelift-only with an optional LLVM pass, or two-tier from day
   one? The former is much less work and keeps the default build honest.

## 7. Suggested next step

Prototype the narrowest slice that validates the thesis: lexer + LL(1) parser for
items/statements/patterns, per-function bidirectional checker with ADTs and enums,
`.tli` serialization with fingerprinting, and *no* codegen (check-and-emit-summary only).

Measure: LOC/s for parse, LOC/s for check, and incremental no-op rebuild latency. If
parse+check hits 150k LOC/s/core and no-op rebuilds land under 200 ms, every remaining
decision above is a detail rather than a risk.
