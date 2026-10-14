# 11. A generational collector

Written 2026-10-13, on the `native` branch, for review before any code.
It comes before native code (10): order 1 of the review. The collector
changes what stores into the heap must do, and native code would
otherwise have to be revisited.

## Why

Two of the gaps plan/07 found against Gauche are the collector's:

- **Allocation-heavy programs are slow.** `mperm` runs at 0.62 of
  Gauche's speed, `earley` at 0.64, and `destruc` and the `boyer`s lose
  too. Every collection re-marks the whole live heap (03 §4, D3), so a
  program that keeps a large structure live pays for it at every
  collection.
- **Peak memory is large.** `earley` reaches 7.5GB, `mperm` 3.9GB and
  `gcbench` 508MB (09, after `GC_GROWTH` became 2). With collections
  spaced in proportion to the live heap, the heap reaches about
  `1 + GC_GROWTH` times the live data before it is swept. Pages are never
  given back (`Slots`' ponytail note).

Native code doesn't help with either. A generational collector helps
both:

- young objects are collected often and cheaply, without marking the old
  ones;
- the old generation is collected rarely, so the growth factor that
  spaces collections can come down, which brings the peak down with it.

## The constraint: objects don't move

A copying nursery is the usual design, but s3 depends everywhere on
objects staying where they are:

- a `GcRef` is an address, and `Value` holds it;
- the dispatch loop's `Frame` holds `&'static Template` and the closure's
  free-variable slice;
- builtins hold `&Obj` across allocations;
- `eq?` hashing for hash tables (09, "what follows") wants addresses that
  don't change;
- native code (10) relies on values not moving between safepoints.

Making objects move would mean finding and updating every one of those,
or adding a level of indirection everywhere. So this plan keeps objects
in place.

**The design: sticky mark bits.** This is the non-moving generational
scheme of Demers et al. (1990), which Boehm's generational mode and
Ruby's RGenGC also use:

- **An object is young or old.** It is born young. If it survives a
  collection it becomes old, and its mark "sticks": a minor collection
  treats it as already marked and doesn't trace through it.
- **A minor collection:**
  1. marks from the roots and from the remembered set;
  2. stops at old objects;
  3. sweeps only the objects allocated since the last collection, which
     `Heap` keeps in a list (8 bytes per allocation);
  4. turns the survivors old.
- **The remembered set** holds the old objects into which a young
  reference has been stored since the last collection. The write
  barrier adds them, and each one carries a flag so it is added once.
- **A major collection** is today's: mark everything, sweep every page.
  It runs when the old generation has grown by a factor since the last
  one.

All of the existing machinery stays:

- collection only at safepoints;
- explicit roots;
- the worklist;
- `Slots`' pages and free list;
- stress and poison modes.

The epoch mark becomes an epoch plus an age.

## The write barrier

Every store of a `Value` into a heap object goes through one function:

```rust
heap.store(owner, &field, v)   // field.set(v), then: if owner is old and v is a
                               // young object, remember owner
```

Today, a mutable field is a `Cell<Value>` that anyone can `set`. G1
replaces it with a type, `Field`, whose only setter is `Heap::store`, so
**the compiler finds every store**, and a store added later can't skip
the barrier. There are about 20 stores of a `Value` today:

- the dispatch loop: `PatchFree`, `BoxSet`, `GlobalSet`,
  `GlobalDefine`, `VectorSet` and `RecordSet`;
- `set-car!`, `set-cdr!`, `list-set!`, `vector-set!`, `vector-fill!`
  and `vector-copy!`;
- record modifiers;
- binding cells in `globals.rs` and `main.rs`;
- the reader's patching of datum labels (`#0=`);
- one store in the printer.

**Environments** keep their bindings in a Rust hash map, not in fields.
`Env::bind` and `own_cell` call the barrier on the environment object
themselves.

**Objects that are written only while they are made** need no barrier:
continuations, escapes, templates, error objects, record types and
macros. This holds while each is still young when it is filled. `Field`
makes any exception visible.

**Native code** (10) uses the same barrier, through `S3_STORE` in
`s3.h`: the check inline in C, and remembering the object through the
API table.

## What changes

| File | Change |
|---|---|
| `src/heap.rs` | The age bit, the young list, the remembered set, minor and major `collect`, `store`, `Field`, scheduling |
| `src/vm/machine.rs` | The store arms call `heap.store`. `safepoint` is unchanged: it passes the same roots |
| `src/builtins/*.rs`, `globals.rs`, `libraries.rs`, `fasl.rs`, `reader/parser.rs` | Stores go through `heap.store` |
| Everything else | Unchanged: reads are `field.get()` as now |

Nothing changes for Scheme code, the compiler or the boot images.

## Phases

The phases use the same rules as before: each ends with every test
passing in release, debug, and GC stress with poisoning. Speed is
measured in cycles with `/usr/bin/time -l`, against the commit before.

### G0. Measure first

There is no design change in this phase. Under `--features stats`,
collect:

- **allocations by kind,** and bytes including the boxed parts (vector
  contents, strings, bignums);
- **collection time split** into marking, sweeping and dropping objects
  (dropping includes freeing their boxes);
- **survival rate:** the fraction of objects still live at the first
  collection after they were made, which is the generational hypothesis
  for these programs;
- **live data after each collection,** against peak RSS.

The programs: `earley`, `mperm`, `gcbench`, `nboyer`, `paraffins`,
`deriv`, `destruc`, `bench/compile.sh` and `deeprec`.

- **Decides:**
  - whether G2 is worth building: it is if most collection time is
    re-marking objects that survive, and few young objects survive;
  - whether G4 is: it is if pairs are most of the bytes.
- **Exit:** a table of these numbers in this document.

#### G0 results (2026-10-13)

`--features gcstats` (`src/gcstats.rs`), a feature of its own: `stats`
counts every instruction, which would distort the times. The r7rs
benchmarks were run at scale 10, as in 07; peak RSS, cycles and system
time come from `/usr/bin/time -l`.

| | GC % of run (mark + sweep) | survive first GC | old in marking | pairs % | closures % | peak live | peak slots | peak RSS |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| deriv | 19.5 (5.7 + 13.8) | 0.4% | 99% | 82 | 15 | 0.5MB | 2MB | 7MB |
| destruc | 5.3 (1.8 + 3.5) | 1.3% | 96% | 79 | 21 | 0.6MB | 2MB | 7MB |
| paraffins | 26.6 (23.0 + 3.6) | 99.6% | 33% | 50 | 0 (vectors 50) | 742MB | 743MB | 1.2GB |
| nboyer | 9.1 (5.0 + 4.1) | 30.6% | 51% | 100 | 0 | 901MB | 2.5GB | 2.8GB |
| mperm | 9.3 (8.5 + 0.8) | 83.7% | 37% | 78 | 22 | 1.3GB | 1.5GB | 3.9GB |
| gcbench | 11.2 (4.0 + 7.2) | 4.6% | 91% | 0 (records 100) | 0 | 88MB | 264MB | 445MB |
| earley | 27.3 (18.7 + 8.6) | 36.3% | 57% | 70 | 30 | 3.5GB | 6.2GB | 8.1GB |

("Old in marking": the share of all marking spent on objects that had
survived a collection before, which is what a minor collection skips.)

A native profile (`sample`) of the session thread gives the same
picture:

| | dispatch loop (incl. inline allocation) | `collect` | `new_page` | `malloc`/`free`/drop of boxes |
|---|---:|---:|---:|---:|
| mperm | 73% | 11% | 4.5% | under 2% |
| earley | 65% | 15% | 2.5% | about 5% |

Instructions per cycle are 3.5 (earley) to 4.3 (mperm), so the mutator
isn't stalling on memory to any great degree.

**What this says:**

1. **The collector is a smaller part of the time than 07 assumed.**
   Since `GC_GROWTH` became 2 (09), marking and sweeping take 5-27% of
   these programs. A minor collection saves only the old part of the
   marking, plus some of the sweeping. Its upper bound is about:
   - 1.1-1.2x on earley and paraffins;
   - 1.03-1.06x on mperm, gcbench and nboyer.

   G2's 1.5x target can't be reached. mperm's and earley's gap to
   Gauche is mostly the cost of each instruction, which plan/10
   addresses.
2. **Memory is the real problem, and it is the size of objects.** Pairs
   are 70-100% of the objects in every program but two (gcbench is
   records, paraffins half vectors), and each takes an 80-byte slot for
   16 bytes of content. Closures are most of the rest: 80 bytes, plus a
   separately allocated array of free variables (3.1GB in earley).
   - A 16-byte pair with its mark bit in a per-page bitmap would cut
     nboyer's live data about 5 times, and earley's and mperm's about
     3 times.
   - A generational collector would only reduce the slack between live
     data and peak slots: 1.1x (mperm, paraffins), 1.8x (earley),
     2.8x (nboyer).
3. **The young survive poorly in the large programs.** 30-84% of young
   objects survive their first collection in nboyer, earley and mperm,
   so promoting them after one survival would fill the old generation
   with data that dies soon after.
4. **Small heaps collect too often.** deriv collects 3,350 times, each
   time re-marking the ~6,500 objects the system itself holds, because
   `GC_THRESHOLD` is 20,000 objects. At 200,000, deriv takes 1.06x
   fewer cycles, destruc 1.01x and gcbench 0.99x. That's small, but it
   is a one-line change.

**Revised order, for review:**

1. **G4 becomes the first phase, and is broader: compact objects.** Pairs
   go in their own pages of 16-byte slots, with a mark bitmap per page
   and a `Value` tag of their own (tags 5-7 are free). There are 27
   `Obj::Pair` match sites. Closures with their free variables inline
   come next, if pairs alone show that compact objects pay.
   - **Target:** peak RSS halved or better on earley, mperm and nboyer.
   - **To measure:** cycles, which could gain from better cache use or
     lose to the second kind of slot.
2. **`GC_THRESHOLD` raised**, measured on `bench/` and the r7rs set.
3. **G1-G3 (the barrier and minor collections) deferred.** They are
   worth doing only if, after compact objects, collection time is still
   a large share in programs that matter. G0's numbers suggest 1.05-1.2x.
   The barrier rule in plan/10 (`S3_STORE`) stays: it costs nothing, and
   it keeps the generational collector possible.

### G1. The barrier, doing nothing

- `Field` replaces `Cell<Value>` in heap objects.
- Every store goes through `heap.store`, which only stores for now.
- The environment hooks are added.
- **A checking mode, `S3_GC_VERIFY`.** Before each collection it walks
  the heap and fails if an old object refers to a young one without
  being in the remembered set. Ages exist only in this mode, so the mode
  can find a missed barrier before G2 relies on it.

- **Exit:** every test passes, with stress and with `S3_GC_VERIFY`.
  Cycles are unchanged on `bench/` and the r7rs set (the barrier is an
  inline call that does nothing yet).

### G2. Minor collections

Add the age, the young list, the remembered set, minor collections, and
promotion after one survival. Major collections are today's collector.

- **Scheduling:**
  - a minor collection after `NURSERY` allocations;
  - a major collection when the old generation reaches `GC_GROWTH` times
    its size after the last major.
- **Roots in a minor collection** are those `safepoint` passes, plus the
  remembered set, `pinned` and the libraries. The symbol table doesn't
  need scanning: symbols are old and point at nothing.
- **Stress mode** makes a minor collection at every safepoint and a major
  collection at every hundredth. `S3_GC_VERIFY` runs in the test suite's
  stress run.

- **Exit:** every test passes in stress, poison and verify modes, and so
  does chibi's suite. Measured on the allocation-heavy benchmarks above
  and on the vector loops (`array1`, `puzzle`, `pnpoly`), where the
  barrier costs most.
- **Target, to confirm after G0:** `mperm`, `earley` and `gcbench` at
  least 1.5x faster in cycles, and no benchmark more than 3% slower.

### G3. Memory

- `NURSERY` and the major growth factor are set by measuring.
- Empty pages are given back to the system after a major collection,
  removing `Slots`' ponytail note.

- **Exit:** peak RSS for `earley`, `mperm` and `gcbench` measured against
  today's. **Target:** half or less, with G2's speed kept.

### G4. Smaller pairs (if G0 says so)

Every object is 80 bytes, the size of the `Obj` enum, so a pair takes
five times its 16 bytes of content. If pairs are most of the bytes, they
move to their own pages of 24-byte slots (a mark word, the car and the
cdr), with a `Value` tag of their own. This is a larger change: every
`Obj::Pair` match becomes `Value::as_pair`. It is done after G2, so the
collector already handles two kinds of slot ages and the remembered set
when it gains a second slot size.

- **Exit:** peak memory and cycles, against G3.

### G5. Only if measured

- **Scanning only part of the stack.** A minor collection scans the
  whole value stack. A deep recursion (`deeprec`) then pays for its
  depth at every minor collection. The fix is to keep a mark of how
  much of the stack hasn't changed since the last collection and scan
  only above it.
- **Card marking for large vectors.** An old vector written in a loop
  is remembered whole and rescanned at every minor collection. The fix
  remembers only the parts of the vector written.

## Decisions to confirm

| # | Decision | Recommendation |
|---|---|---|
| G-a | Moving or not | Non-moving: sticky mark bits |
| G-b | Finding young objects to sweep | A list of objects allocated since the last collection, not a sweep of every page |
| G-c | Promotion | After one survival. An age counter only if G0 or G2 shows old-generation garbage from objects promoted too early |
| G-d | The barrier | An object-granular remembered set, with a flag per object. Card marking only if G5 shows it |
| G-e | Enforcing the barrier | A `Field` type whose setter is `Heap::store`, plus `S3_GC_VERIFY` |
| G-f | Smaller pairs | Only if G0 shows pairs dominating the bytes |
| G-g | Branch | Implemented on its own branch, `collector`, from `main`. `native` keeps the plans until then |

## Risks

- **A missed barrier frees a live object.** This is the classic
  generational bug, and it shows up long after the store that caused it.
  The defences, in order:
  1. `Field` makes missing a barrier a compile error;
  2. `S3_GC_VERIFY` checks the invariant at every collection;
  3. stress mode makes minor collections constant;
  4. poison mode makes a premature free visible.
- **The barrier's cost in tight store loops** (`vector-set!` in
  `array1`, `puzzle`, `pnpoly`). The fast path is two tests: is the owner
  old, and is the value a young object. G2 measures it. If it costs more
  than a few percent, the test can be inlined into the `VectorSet` arm
  and the slow path moved out of line.
- **Rust code holding a young `GcRef` that isn't a root.** The contract
  is unchanged: a `GcRef` is valid while it is reachable from the roots
  at every safepoint. Minor collections only make a violation show up
  sooner.
- **The dispatch loop's code generation** (09): store arms change in G1.
  Cycles are checked there, before any collector change, so a slowdown
  from code layout is told apart from one from collecting.

## Reading

- Alan Demers, Mark Weiser, Barry Hayes, Hans Boehm, Daniel Bobrow, Scott
  Shenker, *Combining Generational and Conservative Garbage Collection*
  (POPL 1990). The sticky-mark-bit scheme.
- Koichi Sasada, RGenGC (RubyKaigi 2013). The same scheme added to an
  interpreter whose C extensions assume objects don't move, with a write
  barrier introduced gradually.
- Richard Jones, Antony Hosking, Eliot Moss, *The Garbage Collection
  Handbook* (2nd ed., 2023): the chapters on generational collection and on
  write barriers.
