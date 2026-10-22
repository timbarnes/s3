# 12. Development tools: source locations, help, trace

Written 2026-10-22, for review before any code. It comes before native
code (10), which is ready to start now that the memory work (11, G0-G3)
is done.

**Status:** T1-T3 done 2026-10-22. Before it, a string before a body's
internal definitions was made a docstring, dropped for now
(`body-in-frame`, `scheme/compiler/expand.scm`); T3 stores it.

## Why

Working in s3 today means Emacs (`M-x run-scheme`, which works), the
R7RS report and s3's `docs/`. When something goes wrong:

- **A run-time error names procedures, not places.** The backtrace is
  `in g`, `in f`. There's no file, no line, and nothing at all for an
  anonymous procedure or a top-level form.
- **A compile-time error names neither.** `define: only allowed at top
  level or at the start of a body: (define b 2)` leaves it to you to
  find that form.
- **There's no help at the REPL.** s1 had `(help 'name)`. s3 never did,
  so checking an argument order means opening the report.
- **There's no tracer.** Finding which call went wrong means adding
  `display`s.

These are also the tools the native work will need. Plan 10 keeps each
native template's bytecode and its `ip`s so that "backtraces and error
positions" keep working. s3 has no error positions yet: T1 and T2 make
them, and native frames then get them for free.

## What s3 has to build on

- **Templates** (`src/vm/op.rs:184`) have a name, parameter counts, code,
  constants, and the names of locals and free variables, for `dis`.
  They're made from `TemplateFields` (`src/compile/data.rs:177`) after
  the verifier passes them, and written to compiled files by
  `src/fasl.rs` (format version 2).
- **Frames** (`src/vm/machine.rs:58`) save the caller's closure and `ip`.
  `record_backtrace` turns them into names.
- **The reader** (`src/reader/`) builds plain pairs and keeps no
  positions. The tokenizer reads characters from the port and keeps no
  line count. `parse` makes a new tokenizer for every datum, so a line
  count has to live in the port.
- **The AST** (`scheme/compiler/ast.scm`) is tagged vectors, and only
  `ast.scm` indexes them, so adding a field to one kind of node touches
  that file and the places that construct it.
- **Objects don't move** (11, "The constraint: objects don't move"), so a
  pair's address names it for as long as it's live.
- **`(library-exports '(scheme base))`** lists a library's names, which
  lets a test check that every standard name has help.

## Phases

| Phase | What | Size |
|---|---|---|
| T1 | Positions from the reader; compile-time errors say `file:line` | small-medium |
| T2 | Line tables in templates; run-time errors and backtraces say `file:line` | medium |
| T3 | Docstrings kept; `help` and `apropos` | small-medium |
| T4 | `trace` and `untrace`, in Scheme | small |

Each phase is useful on its own. T1 and T2 are the most valuable, and T2
is the only one with real risk. T3 and T4 could run in either order.

### T1. Positions from the reader

**Line counts in ports.** A textual input port counts the newlines it
reads, with `unread_char` taking one back. The REPL's port is standard
input, so REPL forms get line numbers too. `-e TEXT` counts within
`TEXT`.

**A position table, used only while compiling.** When the compiler
reads source (`read_program`, `%read-library-file`, the REPL), the
parser records each list's starting line in a table on the heap, keyed
by the pair's address. `(read)` from a user program records nothing, so
it gets no slower and the table doesn't grow. The table is cleared after
each top-level form (REPL) or each file. Its file name is set when it's
started: a path, `repl` or `-e`.

**The expander keeps a current position.** A primitive,
`(%source-line pair)`, looks a pair up and returns its line, or `#f`.
`expr` checks each list form it expands. If the form has a line, that
line becomes the expander's current position while that form is
expanded. A form a macro made has no entry, so it keeps the line of the
nearest source form around it: the macro's use. That's the right answer
for an error in a macro's output.

**Compile-time errors.** `fail` prefixes the current position:

```
error: prog.scm:14: define: only allowed at top level or at the start of a body: (define b 2)
```

**Tests:** an error from a nested form in a file, from a macro's output
(the use's line), and at the REPL. Also check that `read` in a program
leaves the table empty.

### T1 results (2026-10-22)

As planned, with these details:

- `src/sources.rs` holds the tables (`Heap::sources`). A position is a
  file number and a line packed into a fixnum (`file << 24 | line`), so
  the expander and, in T2, call nodes hold one value. File names live
  for the process.
- The tables are a stack. A program's or REPL form's table is open while
  it is read and compiled. A library file's is open between
  `%library-compile-begin` and `-end`, and `load`'s while it runs its
  forms, which it holds. A lookup searches every open table, and
  `include` records into the innermost, with the included file's name.
  An error closes them all (`abandon_compiles`).
- `parse_source` records; `parse`, which `read` uses, doesn't.
- The REPL's file is `stdin` and `-e`'s is `-e`.
- **Not in the plan:** `expr` isn't the only way into a form.
  `body-in-frame` expands body forms' heads itself, and expands a
  definition's value only after it has found all the definitions. So it
  sets each form's position itself, and keeps each definition's position
  for when its value is expanded.
- **Compile time:** `bench/compile.sh --compare` against the commit
  before: `repl` 0.97x, `chibi` 0.96x, `compiler` 1.00x. The `compiler`
  case reads with `parse`, so it records nothing.
- **Found, for T2:** an error in a form that `load` compiles is raised
  inside `eval`, at run time. So its backtrace lists the compiler's
  own procedures (`in if-form`, `in expr`, ...). That was already so
  before T1.

```
error: p2.scm:5: let: bad binding: (y)
```

### T2. Line tables in templates

**Call nodes carry a line.** `#(call f (arg ...))` becomes
`#(call f (arg ...) line)`, with `line` the expander's current position,
or `#f`. Calls are where run-time errors come from, including calls to
primitives that codegen turns into instructions such as `car`. Other
nodes don't need lines.

**Codegen builds a line table.** When codegen emits a call, or the
instruction it inlines a primitive call to, and the line differs from
the last one recorded, it adds `(ip . line)`. `Template` gets:

- `file: Box<str>`, empty when unknown;
- `lines: Vec<(u32, u32)>`, sorted by `ip`.

The line of an `ip` is that of the last entry at or before it. The
compiled-file format goes to version 3, and the boot images are rebuilt.

**Run-time errors.** `record_backtrace` records the place along with
each name:

- for the frame that raised the error, its current `ip`;
- for each frame waiting for a value, the `ip` of its call.

It also stops leaving out anonymous procedures: they now have a place to
show.

```
error: car: not a pair: 5
  in g (prog.scm:22)
  in a lambda (prog.scm:9)
  in f (prog.scm:31)
```

**`dis`** prints the line beside each instruction where it changes.

**Tests:** a run-time error in a nested call, through a `map` closure,
and from a compiled `.s3o` file (so the table survives fasl). Also check
that the golden `dis` listings change only by the added lines.

**What to check while doing it:**

- **The inliner** (`inline.scm`). An inlined body keeps its own call
  nodes, and their lines, which is right as long as it came from the
  same file. Check that `inline-body` never inlines across files. If it
  can, line entries need a file too.
- **The analyzer and the inliner** match on call nodes' shape, so each
  needs the extra field.
- **Benchmarks.** There's no run-time cost: the tables are read only on
  an error. Measure compile time and the boot images' size anyway, and
  record them here.

### T2 results (2026-10-22)

As planned, with these changes:

- **Global references have positions too.** `unbound variable` is raised
  by `global-ref`, not by a call, and is one of the commonest errors.
  So `global-ref` nodes, like calls, may carry a position. Calls `cond`'s
  `=>` and `case` build in `derived.scm` get the derived form's position.
- **The position is an optional last field.** A node made without one
  is compiled at the position of the call it's inside, so the places
  that build nodes and don't care needn't change. The ones that rebuild
  calls had to keep it: `split-letrecs`, which codegen runs first, and
  the inliner's walk and copy (`make-call`, `node-position` in
  `ast.scm`).
- **No file per template.** A position names its file, so an inlined
  body or an included file's code keeps the right file. The inliner
  question in the plan doesn't arise.
- **Codegen** tracks the innermost position, and `emit` records a change.
  It never records "unknown": code outside every positioned node (a
  `begin`'s `pop`s) can't raise an error. `Template::lines` is
  `Vec<(ip, position)>`. `%make-template` takes it as an optional ninth
  argument, so the old compiler image could still build the new one.
- **Compiled files** are version 3. Each template lists the files its
  line table names, then `(ip, file, line)` entries, since a position's
  file number is only meaningful in one process. Rebuilding the images
  needed the binary to read version 2 for one round (the images' line
  tables are empty, so nothing was lost), as `rebuild-images.sh` says.
- **Backtraces**, as planned: `in g (prog.scm:3)`, `in a lambda (...)`,
  `in the top level (...)`. A frame's `ip` is one past the instruction
  that called or raised, so the position is that of `ip - 1`. A tail
  call leaves no frame, so a procedure or top-level form that ended in a
  tail call isn't shown.
- **`eval`'s compile errors** are caught and raised again outside the
  compiler (`compile-for-eval`, `driver.scm`), so a form `load` compiles
  no longer shows the compiler's procedures in its backtrace.
- **`dis`** prints `; file:line` before the first instruction from each
  place. The golden test runs `s3` in `tests/golden`, so the listings say
  `begin.scm:2`, not a path on one machine. The listings changed only by
  those lines.
- **Cost:** run time unchanged (`bench/bench.sh`, every workload 1.00x).
  Compile time, `bench/compile.sh` against T1: `repl` 0.97x, `chibi`
  0.96x. T1 and T2 together: 0.92-0.93x. Before codegen stopped making a
  closure per call node it was 0.90-0.91x. The boot images' line tables
  are empty, so they didn't grow.

```
error: car: expected a pair, got 3
  in g (prog.scm:3)
  in loop
```

The `in loop`s are `map`'s, from `core.scm`, which has no positions.

### T3. Docstrings, `help` and `apropos`

**Docstrings are kept.** `lambda-form` takes a leading string out of the
body, if more forms follow it, and passes it on as the lambda node's
documentation. `body-in-frame` keeps dropping strings among a `let`'s
definitions. The lambda node gets a `doc` field, and `Template` a
`doc: Box<str>`, written to compiled files. That makes a second format
change: version 4, or fold it into T2's version 3 if T2 hasn't been
merged yet.

**Help for the built-in procedures comes from `docs/`.** The docs
already list each procedure as a bullet:

```
* `(memq obj list)`, `(memv obj list)`, `(member obj list [compare])`: the first sublist of ...
```

A build step (`build.rs`, or a script whose output is committed)
extracts every bullet that begins with a signature into a table. Each
name maps to its signature, the bullet's text, and its page and section.
There's one copy of the text, and fixing the docs fixes `help`. s1
instead kept help strings in its Rust source, and those drifted from its
docs.

A test runs `library-exports` over every `(scheme ...)` library and
fails for a name with no help. That test also shows where the docs have
gaps.

**`(help x)`** displays the help text and returns nothing. That suits the
REPL, and avoids s1's escaped string with `\n` in it. `x` may be:

- **a symbol:** looked up in the interaction environment. A procedure is
  then treated as below. A built-in name or syntax keyword uses the
  table.
- **a procedure:** its docstring, or the table's entry for a built-in.
  Either way it also shows a signature made from the template's
  parameter names (`local_names` has them) and, after T2, the file and
  line where it was defined.

```
> (help 'member)
(member obj list [compare])
  The first sublist of list whose car is obj, or #f. ...
  See docs/pairs-and-lists.md#searching
```

**`(apropos "str")`** lists the names in the interaction environment
that contain `str`. This needs a primitive that lists an environment's
names, if there isn't one already.

Both go in `(s3)` and are listed in `docs/extensions.md`.

**Tests:** help for a built-in, a syntax keyword, a procedure with a
docstring, one without (signature only), and one loaded from a compiled
file. Also the coverage test above.

### T3 results (2026-10-22)

As planned, with these details:

- **Docstrings** go on the lambda node, with where the procedure was
  defined (the expander's current position): `lambda-doc`,
  `lambda-position`. The analyzer and inliner rebuild lambdas with
  `lambda-with-body`, which keeps both. `Template` has `doc` and
  `position`, and `%make-template` takes them as optional arguments 10
  and 11. Compiled files are version 4: T2's version 3 had been
  committed, so a file written by it is refused rather than misread.
  The boot images needed one round of reading version 3, as in T2.
- **The manual is built in.** It's included with `include_str!` and parsed
  on the first `help`, in `src/help.rs`. That's simpler than a build
  step, and the binary grows by the manual's size: 2.00MB to 2.14MB.
- **The extractor's rules.** A line, bullet or paragraph, that starts
  with signatures (`` `(name ...)` ``, separated by commas) documents
  each of those names. A paragraph of signatures alone also takes the
  paragraph after it, since that's how the syntax is documented
  (`` `(cond clause ...)` `` then its explanation). A name no line starts
  with takes the first line that shows it. Among several lines that
  start with a name, the first wins, in page order.
- **What the coverage test found**, fixed in the docs: 51 names had no
  entry. `caar` ... `cddddr` were described in words, now in three
  bullets of signatures. `char?` was missing. `eof-object`,
  `exact->inexact`, `quotient` and the like, `letrec*`, `let*-values`,
  `string-ci<?` and the like, `quasiquote` and `unquote`, and the R5RS
  environments were mentioned without signatures. Some bullets started
  with signatures without documenting them. Those are the REPL notes
  on `(define car ...)` and the `define-library` and `cond-expand`
  declaration lists, which made `or` cond-expand's. They now start with
  the keyword.
- **Auxiliary syntax** (`else`, `=>`, `_`, `...`) has no signature, so
  `help` shows the form it belongs to (`AUXILIARY` in `help.rs`).
- **`help`** writes to the current output port (so it can be captured)
  and returns nothing. A procedure without a docstring whose name is in
  the manual, such as `map`, written in `core.scm`, gets the manual's
  entry.
- **Cost:** compile time 0.97-0.99x against T2.

```
> (help 'member)
(memq obj list), (memv obj list), (member obj list [compare]): the first sublist of list whose car is obj, or #f. ...
  Pairs and Lists, Searching: https://timbarnes.github.io/s3/pairs-and-lists/#searching
> (define (sq x) "The square of x." (* x x))
> (help sq)
(sq x)
  The square of x.
  Defined at stdin:1.
```

### T4. `trace` and `untrace`

`(trace 'f)` rebinds the global `f` to a wrapper procedure. The wrapper
prints the call, indented by depth, calls the original, then prints the
result. `(untrace 'f)` puts the original back, and `(untrace)` does that
for everything traced. It's written in Scheme in `core.scm`, about 40
lines, with no VM change: so it works the same for native procedures
later.

```
> (trace 'fact)
> (fact 3)
(fact 3)
  (fact 2)
    (fact 1)
    => 1
  => 2
=> 6
```

**Limits.** These are documented, not fixed:

- **Only calls through the global are traced.** Calls the inliner
  inlined, `SelfTailCall` loops, and local procedures are not. Tracing
  is for the REPL and for procedures loaded without inlining.
- **A traced procedure's tail calls stop being tail calls**, because the
  wrapper waits for the result. A traced loop of a million iterations
  can run out of stack. The error says so rather than crashing.

A VM-level tracer, with a flag on each template checked at `Call`, would
lift the first limit. It costs a test on every call and needs a matching
hook in native code. Leave it until the wrapper falls short.

**Tests:** the output above, `untrace` restoring the original procedure,
and tracing a procedure that raises an error (the depth must reset).

## Decisions

| Question | Choice | Why |
|---|---|---|
| Where positions live while compiling | A table keyed by pair address, beside the datum | Syntax objects that carry positions (as in Racket) would change the reader, the expander and `syntax-rules` throughout. A table changes nothing for code that doesn't ask for it |
| Which nodes carry lines | Calls only | Errors come from calls. Fewer fields means fewer places to change and smaller tables |
| Where lines are stored | A table of `(ip, line)` per template, read only on an error | No run-time cost. Native code (10) keeps `ip`s, so it works there too |
| Where built-in help comes from | Extracted from `docs/` | One copy of the text |
| How tracing works | Rebinding globals to wrappers, in Scheme | No VM or native-code change. Its limits are acceptable for debugging |
| Not done | A stepping debugger and breakpoints, Geiser or SLIME integration, a debugger at the error prompt | s1 had a stepper, and it was a large share of its code. Look again after T4 if these are missed |

## Risks

- **A stale entry in the position table.** If a source pair were freed
  during a compile and its slot reused, a macro's pair could be given
  its line. The forms being compiled are live until the table is
  cleared (the driver holds them), so this shouldn't happen. A test with
  `S3_GC_STRESS=1` checks it.
- **T2 touches the compiler's core.** That's the call node shape, then
  codegen, the inliner and the analyzer, followed by a fasl version and
  a boot image rebuild. The golden listings and the fixed point of
  `scripts/rebuild-images.sh` are the checks.
- **The docs' bullets aren't uniform.** Some cover several procedures in
  one line, or say "every combination up to four levels deep"
  (`cadr` ...). The extractor maps every name a bullet shows in a
  signature to that bullet. The coverage test finds the rest, and these
  are fixed in the docs rather than in the extractor.

## Reading

- `plan/10-native.md`: why native code keeps `ip`s and bytecode.
- `plan/11-collector.md`: why objects don't move.
- s1's `docs/extensions.md` (`help`, `add-doc`) and `src/debugger.rs`:
  what s1 offered, and how much the stepper cost.
