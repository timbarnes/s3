# 10. Native code: bytecode to C, loaded with dlopen

Written 2026-10-13, on the `native` branch, for review before any code.
**Status:** not started. The memory work in 11 (G0-G3) is done.
06 §5 recorded native code as a later direction and said that, with the
compiler in Scheme, a C backend was the natural route. This document
chooses a design and splits it into phases N0-N6. It follows the memory
work in 11, which comes first. Its stores go through a barrier macro, so
a generational collector (11, G4) stays possible.

## The approach in one paragraph

A new library in Scheme, `(s3 native)`, takes **finished, verified
templates** and writes C: one C function per template, with each bytecode
instruction translated in place. `cc -shared` builds a shared library, and
s3 loads it with `dlopen`. Loading attaches each C function to its
template. The C code runs on **s3's own value stack and frame stack**. It
never calls a Scheme procedure itself: a call returns to the dispatch
loop, which carries it out as it does now. So:

- tail calls,
- `call/cc`,
- the collector's roots,
- errors,
- backtraces

all keep working unchanged. Bytecode and native procedures call each
other freely. The compiler isn't changed.

## Why this shape

| Question | Choice | Why |
|---|---|---|
| What the backend reads | Templates (bytecode), not the AST | The compiler stays untouched. Any template can be translated: from the REPL, a `.s3o` file or the boot images. Each instruction's C can be checked against the dispatch loop's arm for it. The verifier already works out the stack depth at every instruction, which turns stack slots into C variables (N3) |
| Language of the backend | Scheme | The compiler is in Scheme (U1, P13). It emits text, which needs no bindings to a code generator (06 §5) |
| Target | C, through the system's `cc` | Portable to wherever s3 builds, including targets Cranelift lacks (06 §5). The C compiler does register allocation and instruction selection |
| Loading | `dlopen` at run time; static linking ahead of time (N5) | No relinking for the REPL or a library loaded on demand |
| Runtime symbols | A table of function pointers passed to the module's init function | s3 exports no symbols, so there are no `-export_dynamic` or `-undefined dynamic_lookup` flags, and the same model works on Windows if s3 ever goes there |
| Calls | Through the dispatch loop (a trampoline) | No C stack frames between Scheme frames, so `call/cc` still copies only s3's stacks, and every collection happens where all live values are on the value stack. Tail calls are guaranteed without `musttail` |
| Collector roots | Unchanged | The collector runs only at safepoints, on entry to `Call` and `TailCall` (03 §4). Native code reaches those only by returning to the loop. Between them, C may keep values in registers: nothing can move or free them. No stack maps are needed |

The cost of the trampoline: each call and return passes through the
dispatch loop, so call-bound code (`fib`, `tak`) gains least. N6 reduces
this for calls the compiler knows.

## How it fits the VM

**One new instruction, `Native`.** A native template keeps its bytecode,
but its first instruction, and the instruction after each non-tail call,
is replaced with `Native`. These are the points where execution enters
the procedure. The dispatch loop's arm for `Native` calls the template's
C function, passing the `ip` it entered at. The C function `switch`es on
that `ip` to the matching point in its code. So:

- a saved `Frame`'s `ip` means what it means now;
- continuations, escapes and backtraces need no change;
- the rest of the bytecode stays, for `dis` and for error positions.

**The C function's ABI:**

```c
typedef uint64_t s3_value;           /* the NaN-boxed Value, value.rs */

typedef struct {
    s3_value *stack;                 /* the stack's base */
    uint32_t sp, fp;                 /* stack pointer, frame pointer */
    s3_value *free;                  /* the closure's free variables */
    const s3_value *consts;          /* the template's constants */
    uint32_t ip;                     /* in: where to enter; out: the instruction to act on */
    uint32_t argc;                   /* out: for CALL and TAILCALL */
    s3_value error;                  /* out: for ERROR */
    void *heap;
    const struct s3_api *api;        /* builtins, allocation, errors */
} s3_regs;

enum { S3_RETURN, S3_CALL, S3_TAILCALL, S3_ERROR };
typedef int (*s3_native)(s3_regs *r);
```

The function runs until its next call, tail call, return or error. It
then writes `sp` and `ip` back and returns what the loop should do next.
The `Native` arm then does exactly what the `Call`, `TailCall` or
`Return` arm does now, or what `fail!` does. Those paths are reused, not
copied.

**What the C does without the loop:**

- constants, locals, free variables, boxes and globals;
- jumps, including `SelfTailCall`, which becomes a `goto` to the top;
- the inlined primitives, with the same fast paths. Their slow paths call
  the builtin through the table, so results and errors match.

**Making closures** calls the heap through the table. Allocation never
collects (03 §4), so values held in C variables stay valid.

**Every store goes through the barrier.** A store of a value into a heap
object (`BoxSet`, `GlobalSet`, `GlobalDefine`, `PatchFree`, `VectorSet`,
`RecordSet`, and any later one) is written in the generated C as
`S3_STORE(owner, field, v)`, never as a plain assignment. In `s3.h`,
that macro is a plain store until a generational collector exists (11,
G4). Then it also does the check `Heap::store` does: if the owner is old
and the value is a young object, it remembers the owner through the API
table. This rule holds from N0's hand-written C onwards, and the emitter
has one place that writes stores.

**`s3.h`** is written by hand and lives in `src/native/`. It is built into
the binary with `include_str!` and written beside the generated C, so a
user needs only a C compiler. A test checks that its layouts and tags
agree with Rust (`s3_regs` is `#[repr(C)]` on the Rust side).

## What changes in existing code

The work is additive, as the review asked: new files, and a new step
after the compiler rather than a change to it. Some existing files need
small hooks:

| File | Change |
|---|---|
| `src/vm/op.rs` | `Op::Native`, added last. `Template` gains `native: Option<s3_native>` |
| `src/vm/machine.rs` | One match arm. Bytecode-only programs should be unaffected; N1 checks this in cycles, because the loop's code generation is fragile (09) |
| `src/vm/verify.rs` | Accept `Native` in place of the instruction it replaced, at the same depth |
| `src/fasl.rs` | Nothing at first: native code isn't written into `.s3o` files. The shared library sits beside them, in the same cache (N4) |
| `src/compile/data.rs` | `%template-parts`, the inverse of `%make-template`, so Scheme can read a template's code. Also `%template-depths`, the verifier's stack depths |
| `scheme/compiler/*` | Nothing. `(s3 native)` is a separate library, loaded only when used, so the boot images don't change until N5 |

The new files are `src/native/mod.rs` (the API table, `%native-load`,
running `cc`), `src/native/s3.h`, `scheme/native/*.scm` (the emitter),
and tests.

## Phases

Each phase ends with every test passing, as before. Each one that should
change speed is measured in cycles (`/usr/bin/time -l`), as in 09.

### N0. A spike, by hand

Write the C for `fib`'s template by hand. Call it from a temporary
`Native` arm through `dlopen`, and measure. This is the go/no-go point:
it shows the trampoline's cost before anything is built on it.

- **Exit:** `fib` and a loop (`sum`) timed against bytecode. Proceed if
  the loop gains at least 2x and `fib` doesn't lose.
- **Teaches:** the ABI, `dlopen` on macOS (code signing), and the real
  cost of a round trip through the loop.

### N1. The runtime side

Add:

- `Op::Native` and `Template.native`;
- the `Native` arm, reusing the call, return and error paths;
- `s3.h` and the API table;
- `%native-load path`, which runs `dlopen`, calls `s3_init(&api)`, and
  gets back the module's table of functions;
- `%template-parts` and `%template-depths`;
- `dlopen` and `dlsym` declared as `extern "C"`, with no new crate.
  `libloading` can come in if Windows ever matters.

Tests use C modules written by hand.

- **Exit:** a hand-written module's procedures call bytecode ones, and
  bytecode ones call them, including through `call/cc` re-entry, an
  escape, an error caught by `guard`, and a collection under GC stress.
  `bench/` shows no change in cycles for bytecode.

### N2. The emitter: every instruction, values on the real stack

`(s3 native)` translates a template to C, one instruction at a time, as
the dispatch loop's arm for it does: pushes and pops go to `stack[sp]`.
This is the "baseline" level in 06 §5, but compiled by the C compiler
rather than copied and patched.

Adds:

- `(native-compile-file "x.scm")`, which compiles to `x.s3o` and
  `x.dylib` (`.so` on Linux), and `--native` on the command line;
- **`S3_NATIVE=1`, which compiles every template loaded to native code,
  for testing.** This runs the whole suite through the new path.

Error positions: a failing instruction reports its bytecode `ip`, so
backtraces and messages are the same as for bytecode.

- **Exit:** with `S3_NATIVE=1`, every test, chibi's suite (1154) and the
  r7rs benchmarks give the same results as bytecode, in release, debug,
  and GC stress with poisoning. The generated C compiles with
  `-Wall -Werror`. Speed is measured on `bench/` and the r7rs set.
- **Estimate:** 1.5-2x on loops; call-bound code about even.

### N3. Temporaries in C variables

The depths from `%template-depths` name each stack slot at each
instruction, so temporaries become C locals (`s0`, `s1`...), and the C
compiler keeps them in registers. They are written to the real stack only
where the loop or the collector will look: before a call, a return, an
error, or a `MakeClosure` that captures them. Locals (`fp + i`) stay on
the stack at first. Caching unassigned ones in C variables between calls
is the second half of the phase, measured separately.

- **Exit:** as N2. Measured against N2.
- **Estimate:** a further 1.5-2x on loops (`sum`, `array1`, `puzzle`).

### N4. Caching and libraries

A library's shared library is cached beside its `.s3o`, under the same
freshness rule, so `cc` runs once per change. Platform differences live
in `src/native/mod.rs`:

- the C compiler (`$CC`, else `cc`);
- the flags: `-shared -fPIC` on Linux, `-dynamiclib` on macOS;
- the suffix.

Native compilation stays opt-in: `cc` takes 50-200ms, too slow for every
REPL line.

- **Exit:** a library compiled natively once loads from the cache with
  no `cc` run. A missing compiler gives a clear error, and bytecode still
  works.

### N5. Native boot images, linked statically

`core.scm` and the compiler are translated to C once, and the C is
committed beside `boot/`. `build.rs` compiles it with the `cc` crate and
links it statically. The images' templates find their functions by index
at startup, with no `dlopen`. The compiler then runs natively: this is
where native code pays the REPL back. This is the ahead-of-time mode, and
it is the same C as `dlopen` uses.

- **Exit:** `rebuild-images.sh` also regenerates the C, and the
  fixed-point test covers it. Compile times are measured with
  `bench/compile.sh`.

### N6. Direct calls within a module

This is optional, and done only if N2-N5 show calls dominating. A
`GlobalRef` and `Call` to a procedure defined in the same library, which
step 3 of 09 already proves is never assigned, can skip the dispatch
loop. Gambit's way is to put a module's procedures in one C function, so
such a call is a `goto` with a frame pushed. `musttail` is a
platform-dependent alternative for tail calls.

- **Estimate:** call-bound code 1.5-2x.

Not planned: an AST-level backend with type and representation analysis
(06 §5's "optimising" level). It would be reconsidered only if N3's
profile shows boxing or type checks dominating.

## Decisions to confirm

| # | Decision | Recommendation |
|---|---|---|
| N-a | Input to the backend | Templates, not the AST |
| N-b | Calls | A trampoline through the dispatch loop; frames stay on s3's stacks |
| N-c | Entering native code | `Op::Native` at the entry and at each return point, keyed by the bytecode `ip` |
| N-d | Runtime symbols | A function table passed to `s3_init`; s3 exports nothing |
| N-e | Where native code is stored | A shared library cached beside the `.s3o`; the fasl format doesn't change |
| N-f | When it is used | Opt-in (`--native`, `native-compile-file`); `S3_NATIVE=1` for testing |
| N-g | Platforms | macOS and Linux with clang or gcc. Windows and the ESP32 are not planned (the design doesn't rule them out) |
| N-h | C extensions | `__builtin_add_overflow` and friends, which clang and gcc have, for fixnum arithmetic |
| N-i | Stores | Always `S3_STORE`, from N0 on: a plain store now, the barrier if 11's G4 is built |

## Risks

- **The dispatch loop's code generation** (09: unrelated changes moved
  `array1` and `sum` by 10%). The `Native` arm is checked in cycles in N1.
  If it costs bytecode, the arm moves out of line.
- **Re-entry from Rust into Scheme.** Builtins that call back into Scheme
  already handle it. Native code never does, by construction.
- **Templates changed after they start running.** Native code is attached
  at load, before a template runs. `S3_NATIVE=1` in N2 attaches it before
  first use, too.
- **A store that skips the barrier** would free a live young object once
  a generational collector exists, and the failure would show up long
  after the store. `S3_STORE` is the only way the emitter writes a
  store, and `S3_GC_VERIFY` (11, G4) would run with the native test
  suite.
- **Diverging semantics.** Each instruction's C is written beside its
  dispatch loop arm, and N2's exit runs the whole suite through native
  code. An inlined primitive's slow path is always the builtin itself.
- **macOS:** freshly built libraries are signed ad hoc by the linker on
  arm64, which is enough for an unsigned s3. A notarised, hardened-runtime
  s3 would need the `disable-library-validation` entitlement.
