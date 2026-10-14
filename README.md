# s3 Scheme

A Scheme compiler, written in Scheme, and a bytecode virtual machine
written in Rust. It implements [R7RS-small](https://standards.scheme.org/corrected-r7rs/r7rs.html),
except for complex numbers.

s3 is a learning project. It continues s2, whose history it keeps:
s2 began with a compiler in Rust, then ported it to Scheme (P13), and
s3 keeps only the Scheme one. That compiler compiles itself, and ships
as boot images built into the binary. s2's last state, with both
compilers, is the tag `s2-final`. Both descend from the interpreter
[s1](https://github.com/timbarnes/s1). The work was done in small
phases, and each one records what it built, what it decided, and what
it measured.

Documentation:

- the [user guide](https://timbarnes.github.io/s3/), with the source in
  `docs/`;
- the [design notes](plan/README.md): the plan, the architecture, and a
  note for each phase;
- the [internal docs](https://timbarnes.github.io/s3/api/s3/), for working
  on s3 itself.

## Getting started

```bash
cargo build --release
target/release/s3                      # the REPL
target/release/s3 prog.scm arg ...     # run a program
target/release/s3 --compile prog.scm   # compile it to prog.s3o
target/release/s3 prog.s3o arg ...     # run the compiled program
target/release/s3 -e '(+ 1 2)'         # evaluate an expression
```

## What it has

- **R7RS-small.** It passes chibi-scheme's R7RS test suite except for
  complex numbers.
  - **Language:** hygienic `syntax-rules` macros, libraries with
    `define-library` and `.sld` files, re-entrant continuations,
    `dynamic-wind`, exceptions, parameters and records.
  - **Data:** exact integers of any size and exact rationals, full Unicode
    characters and strings, bytevectors, and binary and textual ports.
- **A bytecode compiler in Scheme.** Each top-level form is
  macro-expanded, compiled to bytecode for a stack VM and run. `--expand`
  and `--dis` show each step. A verifier checks every compiled procedure
  before it can run.
- **Compiled files.** Programs can be compiled ahead (`--compile`), and
  library files are compiled and cached next to their source.
- **A precise mark-sweep garbage collector**, with pairs, closures and
  records in compact pages.

## Speed

s3 is about 10 times faster than s1. Against established Schemes, measured
on ecraven's r7rs-benchmarks, it is:

- 6 times faster than chibi;
- 1.13 times faster than Gauche;
- half the speed of Chicken and s7, a third of Guile's;
- a fifth to a ninth of the speed of the native-code compilers.

[plan/07-comparison.md](plan/07-comparison.md) has the details.

## Repository

| Path | |
|---|---|
| `src/` | the runtime: reader, VM (`vm/`), heap, builtins, and what the compiler uses of them (`compile/`) |
| `scheme/compiler/` | the compiler |
| `scheme/core.scm` | the derived syntax and the procedures written in Scheme |
| `boot/` | the boot images: `core.scm` and the compiler, compiled; remade by `scripts/rebuild-images.sh` |
| `tests/` | unit, golden, process and Scheme tests, and chibi's R7RS suite (`tests/r7rs/run.sh`) |
| `bench/` | s3 against s1 (`bench.sh`), and against other Schemes (`r7rs/`) |
| `docs/` | the user guide (MkDocs) |
| `plan/` | the design notes |

`cargo test` runs the tests. After changing anything in `scheme/`, run
`scripts/rebuild-images.sh` (a test fails until you do).

The R7RS suite in `tests/r7rs/` is chibi-scheme's, under its own license
(`tests/r7rs/LICENSE-chibi`).
