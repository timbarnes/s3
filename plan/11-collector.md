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
